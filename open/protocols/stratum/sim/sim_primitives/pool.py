# Copyright (C) 2019  Braiins Systems s.r.o.
#
# This file is part of Braiins Open-Source Initiative (BOSI).
#
# BOSI is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
#
# Please, keep in mind that we may also license BOSI or any part thereof
# under a proprietary license. For more information on the terms and conditions
# of such proprietary license or if you have any other questions, please
# contact us at opensource@braiins.com.

"""Generic pool module"""
from .network import Connection, AcceptingConnection
import hashlib
import numpy as np
import simpy
from event_bus import EventBus
from sim_primitives.hashrate_meter import HashrateMeter
import sim_primitives.coins as coins
from .protocol import UpstreamConnectionProcessor


class MiningJob:
    """This class allows the simulation to track per job difficulty target for
    correct accounting"""

    def __init__(self, uid: int, diff_target: coins.Target):
        """
        :param uid:
        :param diff_target: difficulty target
        """
        self.uid = uid
        self.diff_target = diff_target


class MiningJobRegistry:
    """Registry of jobs that have been assigned for mining.

    The registry intentionally doesn't remove any jobs from the simulation so that we
    can explicitly account for 'stale' hashrate. When this requirement is not needed,
    the retire_all_jobs() can be adjusted accordingly"""

    def __init__(self):
        # Tracking minimum valid job ID
        self.next_job_uid = 0
        # Registered jobs based on their uid
        self.jobs = dict()
        # Invalidated jobs just for accounting reasons
        self.invalid_jobs = dict()

    def new_mining_job(self, diff_target: coins.Target, job_id=None):
        """Prepares new mining job and registers it internally.

        :param diff_target: difficulty target of the job to be constructed
        :param job_id: optional identifier of a job. If not specified, the registry
        chooses its own identifier.
        :return new mining job or None if job with the specified ID already exists
        """
        if job_id is None:
            job_id = self.__next_job_uid()
        if job_id not in self.jobs:
            new_job = MiningJob(uid=job_id, diff_target=diff_target)
            self.jobs[new_job.uid] = new_job
        else:
            new_job = None
        return new_job

    def get_job(self, job_uid):
        """
        :param job_uid: job_uid to look for
        :return: Returns the job or None
        """
        return self.jobs.get(job_uid)

    def get_job_diff_target(self, job_uid):
        return self.jobs[job_uid].diff_target

    def get_invalid_job_diff_target(self, job_uid):
        return self.invalid_jobs[job_uid].diff_target

    def contains(self, job_uid):
        """Job ID presence check
        :return True when when such Job ID exists in the registry (it may still not
        be valid)"""
        return job_uid in self.jobs

    def contains_invalid(self, job_uid):
        """Check the invalidated job registry
        :return True when when such Job ID exists in the registry (it may still not
        be valid)"""
        return job_uid in self.invalid_jobs

    def retire_all_jobs(self):
        """Make all jobs invalid, while storing their copy for accounting reasons"""
        self.invalid_jobs.update(self.jobs)
        self.jobs = dict()

    def add_job(self, job: MiningJob):
        """
        Appends a job with an assigned ID into the registry
        :param job:
        :return:
        """
        assert (
            self.get_job(job.uid) is None
        ), 'Job {} already exists in the registry'.format(job)
        self.jobs[job.uid] = job

    def __next_job_uid(self):
        """Initializes a new job ID for this session.
        """
        curr_job_uid = self.next_job_uid
        self.next_job_uid += 1

        return curr_job_uid


class MiningSession:
    """Represents a mining session that can adjust its difficulty target"""

    min_factor = 0.25
    max_factor = 4

    def __init__(
        self,
        name: str,
        env: simpy.Environment,
        bus: EventBus,
        owner,
        diff_target: coins.Target,
        enable_vardiff,
        vardiff_time_window=None,
        vardiff_desired_submits_per_sec=None,
        on_vardiff_change=None,
    ):
        """
        """
        self.name = name
        self.env = env
        self.bus = bus
        self.owner = owner
        self.curr_diff_target = diff_target
        self.enable_vardiff = enable_vardiff
        self.meter = None
        self.vardiff_process = None
        self.vardiff_time_window_size = vardiff_time_window
        self.vardiff_desired_submits_per_sec = vardiff_desired_submits_per_sec
        self.on_vardiff_change = on_vardiff_change

        self.job_registry = MiningJobRegistry()

    @property
    def curr_target(self):
        """Derives target from current difficulty on the session"""
        return self.curr_diff_target

    def set_target(self, target):
        self.curr_diff_target = target

    def new_mining_job(self, job_uid=None):
        """Generates a new job using current session's target"""
        return self.job_registry.new_mining_job(self.curr_target, job_uid)

    def run(self):
        """Explicit activation starts any simulation processes associated with the session"""
        self.meter = HashrateMeter(self.env)
        if self.enable_vardiff:
            self.vardiff_process = self.env.process(self.__vardiff_loop())

    def account_diff_shares(self, diff: int):
        assert (
            self.meter is not None
        ), 'BUG: session not running yet, cannot account shares'
        self.meter.measure(diff)

    def terminate(self):
        """Complete shutdown of the session"""
        self.meter.terminate()
        if self.enable_vardiff:
            self.vardiff_process.interrupt()

    def __vardiff_loop(self):
        while True:
            try:
                submits_per_sec = self.meter.get_submit_per_secs()
                if submits_per_sec is None:
                    # no accepted shares, we will halve the diff
                    factor = 0.5
                else:
                    factor = submits_per_sec / self.vardiff_desired_submits_per_sec
                if factor < self.min_factor:
                    factor = self.min_factor
                elif factor > self.max_factor:
                    factor = self.max_factor
                self.curr_diff_target.div_by_factor(factor)
                self.__emit_aux_msg_on_bus(
                    'DIFF_UPDATE(target={})'.format(self.curr_diff_target)
                ),
                self.on_vardiff_change(self)
                yield self.env.timeout(self.vardiff_time_window_size)
            except simpy.Interrupt:
                break

    def __emit_aux_msg_on_bus(self, msg):
        self.bus.emit(self.name, self.env.now, self.owner, msg)


class Pool(AcceptingConnection):
    """Represents a generic mining pool.

    It handles connections and delegates work to actual protocol specific object

    The pool keeps statistics about:

    - accepted submits and shares: submit count and difficulty sum (shares) for valid
    solutions
    - stale submits and shares: submit count and difficulty sum (shares) for solutions
    that have been sent after new block is found
    - rejected submits: submit count of invalid submit attempts that don't refer any
    particular job
    """

    meter_period = 60

    def __init__(
        self,
        name: str,
        env: simpy.Environment,
        bus: EventBus,
        protocol_type: UpstreamConnectionProcessor,
        default_target: coins.Target,
        extranonce2_size: int = 8,
        avg_pool_block_time: float = 60,
        enable_vardiff: bool = False,
        desired_submits_per_sec: float = 0.3,
        simulate_luck: bool = True,
    ):
        """

        :type protocol_type:
        """
        self.name = name
        self.env = env
        self.bus = bus
        self.default_target = default_target
        self.extranonce2_size = extranonce2_size
        self.avg_pool_block_time = avg_pool_block_time

        # Prepare initial prevhash for the very first
        self.__generate_new_prev_hash()
        # Per connection message processors
        self.connection_processors = dict()
        self.connection_processor_clz = protocol_type

        self.pow_update_process = env.process(self.__pow_update())

        self.meter_accepted = HashrateMeter(self.env)
        self.meter_rejected_stale = HashrateMeter(self.env)
        self.meter_process = env.process(self.__pool_speed_meter())
        self.enable_vardiff = enable_vardiff
        self.desired_submits_per_sec = desired_submits_per_sec
        self.simulate_luck = simulate_luck

        self.extra_meters = []

        self.accepted_submits = 0
        self.stale_submits = 0
        self.rejected_submits = 0

        self.accepted_shares = 0
        self.stale_shares = 0

    def reset_stats(self):
        self.accepted_submits = 0
        self.stale_submits = 0
        self.rejected_submits = 0
        self.accepted_shares = 0
        self.stale_shares = 0

    def connect_in(self, connection: Connection):
        if connection.port != 'stratum':
            raise ValueError('{} port is not supported'.format(connection.port))
        # Build message processor for the new connection
        self.connection_processors[connection.uid] = self.connection_processor_clz(
            self, connection
        )

    def disconnect(self, connection: Connection):
        if connection.uid not in self.connection_processors:
            return
        self.connection_processors[connection.uid].terminate()
        del self.connection_processors[connection.uid]

    def new_mining_session(self, owner, on_vardiff_change, clz=MiningSession):
        """Creates a new mining session"""
        session = clz(
            name=self.name,
            env=self.env,
            bus=self.bus,
            owner=owner,
            diff_target=self.default_target,
            enable_vardiff=self.enable_vardiff,
            vardiff_time_window=self.meter_accepted.window_size,
            vardiff_desired_submits_per_sec=self.desired_submits_per_sec,
            on_vardiff_change=on_vardiff_change,
        )
        self.__emit_aux_msg_on_bus('NEW MINING SESSION ()'.format(session))

        return session

    def add_extra_meter(self, meter: HashrateMeter):
        self.extra_meters.append(meter)

    def account_accepted_shares(self, diff_target: coins.Target):
        self.accepted_submits += 1
        self.accepted_shares += diff_target.to_difficulty()
        self.meter_accepted.measure(diff_target.to_difficulty())

    def account_stale_shares(self, diff_target: coins.Target):
        self.stale_submits += 1
        self.stale_shares += diff_target.to_difficulty()
        self.meter_rejected_stale.measure(diff_target.to_difficulty())

    def account_rejected_submits(self):
        self.rejected_submits += 1

    def process_submit(
        self, submit_job_uid, session: MiningSession, on_accept, on_reject
    ):
        if session.job_registry.contains(submit_job_uid):
            diff_target = session.job_registry.get_job_diff_target(submit_job_uid)
            # Global accounting
            self.account_accepted_shares(diff_target)
            # Per session accounting
            session.account_diff_shares(diff_target.to_difficulty())
            on_accept(diff_target)
        elif session.job_registry.contains_invalid(submit_job_uid):
            diff_target = session.job_registry.get_invalid_job_diff_target(
                submit_job_uid
            )
            self.account_stale_shares(diff_target)
            on_reject(diff_target)
        else:
            self.account_rejected_submits()
            on_reject(None)

    def __pow_update(self):
        """This process simulates finding new blocks based on pool's hashrate"""
        while True:
            # simulate pool block time using exponential distribution
            yield self.env.timeout(
                np.random.exponential(self.avg_pool_block_time)
                if self.simulate_luck
                else self.avg_pool_block_time
            )
            # Simulate the new block hash by calculating sha256 of current time
            self.__generate_new_prev_hash()

            self.__emit_aux_msg_on_bus('NEW_BLOCK: {}'.format(self.prev_hash.hex()))

            for connection_processor in self.connection_processors.values():
                connection_processor.on_new_block()

    def __generate_new_prev_hash(self):
        """Generates a new prevhash based on current time.
        """
        # TODO: this is not very precise as to events that would trigger this method in
        #  the same second would yield the same prev hash value,  we should consider
        #  specifying prev hash as a simple sequence number
        self.prev_hash = hashlib.sha256(bytes(int(self.env.now))).digest()

    def __pool_speed_meter(self):
        while True:
            yield self.env.timeout(self.meter_period)
            speed = self.meter_accepted.get_speed()
            submit_speed = self.meter_accepted.get_submit_per_secs()
            if speed is None or submit_speed is None:
                self.__emit_aux_msg_on_bus('SPEED: N/A Gh/s, N/A submits/s')
            else:
                self.__emit_aux_msg_on_bus(
                    'SPEED: {0:.2f} Gh/s, {1:.4f} submits/s'.format(speed, submit_speed)
                )

    def __emit_aux_msg_on_bus(self, msg):
        self.bus.emit(self.name, self.env.now, None, msg)
