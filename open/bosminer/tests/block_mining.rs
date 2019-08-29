//! The purpose of this test is to verify that the mining functionality of bosminer hasn't been impaired.
//! This test is deterministic - we know hardware can mine all the test blocks in `test_utils`,
//! and we want to verify that we receive correct solution for each block (which tests
//! that all work has been correctly defined and sent to hardware).
#![feature(await_macro, async_await)]

use bosminer::btc;
use bosminer::config;
use bosminer::hal::{self, BitcoinJob, MiningWork, UniqueMiningWorkSolution};
use bosminer::test_utils;
use bosminer::utils;
use bosminer::work;

use btc::HashTrait;

use std::time::{Duration, Instant};
use tokio::timer::Delay;

use futures::channel::mpsc;
use futures::stream::StreamExt;
use futures_locks::Mutex;
use ii_wire::utils::CompatFix;
use tokio::await;

use std::collections::HashMap;
use std::sync::Arc;

use bosminer::misc::LOGGER;
use slog::{error, info, trace, warn};

/// Problem is a "work recipe" for mining hardware that is to have a particular
/// solution in a particular midstate.
/// The `model_solution` is a "template" after which this work is modeled.
#[derive(Clone)]
struct Problem {
    model_solution: UniqueMiningWorkSolution,
    target_midstate: usize,
}

impl Problem {
    fn new(model_solution: UniqueMiningWorkSolution, target_midstate: usize) -> Self {
        Self {
            model_solution,
            target_midstate,
        }
    }
}

impl std::fmt::Debug for Problem {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            fmt,
            "{:?} target_midstate={}",
            &self.model_solution, self.target_midstate
        )
    }
}

/// Problem can be converted to MiningWork.
///
/// The in-soluble midstates (other than the one specified in the problem)
/// are created from the original solution by increasing/decreasing the version
/// slightly. There's no guarantee these blocks have no solution.
impl From<Problem> for MiningWork {
    fn from(problem: Problem) -> Self {
        let job: &test_utils::TestBlock = problem.model_solution.job();
        let time = job.time();
        let correct_version = job.version();
        let mut midstates = Vec::with_capacity(config::MIDSTATE_COUNT);

        // prepare block chunk1 with all invariants
        let mut block_chunk1 = btc::BlockHeader {
            previous_hash: job.previous_hash().into_inner(),
            merkle_root: job.merkle_root().into_inner(),
            ..Default::default()
        };

        // generate all midstates from given range of indexes
        for index in 0..config::MIDSTATE_COUNT {
            // use index for generation compatible header version
            let version = correct_version ^ (index as u32) ^ (problem.target_midstate as u32);
            block_chunk1.version = version;
            midstates.push(hal::Midstate {
                version,
                state: block_chunk1.midstate(),
            })
        }
        MiningWork::new(Arc::new(*job), midstates, time)
    }
}

/// `Solution` represents a valid solution from hardware in a given index.
#[derive(Clone)]
struct Solution {
    solution: UniqueMiningWorkSolution,
    midstate_idx: usize,
}

impl Solution {
    fn new(solution: UniqueMiningWorkSolution, midstate_idx: usize) -> Self {
        Self {
            solution,
            midstate_idx,
        }
    }
}

impl std::fmt::Debug for Solution {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "{:?}", &self.solution)
    }
}

impl From<UniqueMiningWorkSolution> for Solution {
    fn from(solution: UniqueMiningWorkSolution) -> Self {
        let midstate_idx = solution.midstate_idx();
        Self::new(solution, midstate_idx)
    }
}

/// `SolutionKey` is measure by which we pair in problems and solutions
/// If two problems have equal SolutionKeys, they are considered identical.
/// For now we use block hash and midstate index in which the work was solved.
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
struct SolutionKey {
    hash: btc::DHash,
    midstate_idx: usize,
}

impl SolutionKey {
    fn from_problem(p: Problem) -> Self {
        Self {
            hash: p.model_solution.hash(),
            midstate_idx: p.target_midstate,
        }
    }

    fn from_solution(solution: Solution) -> Self {
        Self {
            hash: solution.solution.hash(),
            midstate_idx: solution.midstate_idx,
        }
    }
}

/// `SolutionState` is state of solution in registry.
/// It can be either solved or not solved.
/// When we create a new `SolutionState` (from PRoblem) we attach a job to it so
/// that we can figure out what jobs were not solved.
#[derive(Clone, Debug)]
struct SolutionState {
    solved: bool,
    problem: Problem,
}

impl SolutionState {
    fn new(problem: Problem) -> Self {
        Self {
            solved: false,
            problem,
        }
    }
}

/// Registry holds problems and pairs them with solutions
#[derive(Clone, Debug)]
struct Registry {
    map: HashMap<SolutionKey, SolutionState>,
}

impl Registry {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Adds problem to registry.
    /// Returns true if this problem is unique.
    fn add_problem(&mut self, problem: Problem) -> bool {
        trace!(LOGGER, "adding problem: {:?}", &problem);
        let key = SolutionKey::from_problem(problem.clone());
        if self.map.get(&key).is_some() {
            return false;
        }
        self.map.insert(key, SolutionState::new(problem));
        true
    }

    /// Adds solution to registry.
    fn add_solution(&mut self, solution: Solution) {
        match self
            .map
            .get_mut(&SolutionKey::from_solution(solution.clone()))
        {
            Some(state) => state.solved = true,
            None => warn!(LOGGER, "no problem for {:?}", solution),
        }
    }

    /// Checks if all problems in registry were solved.
    /// Prints the ones that were not solved.
    fn check_everything_solved(&self, print_missing_solutions: bool) -> bool {
        let mut everything_solved = true;
        for (_solution_key, solution_state) in self.map.iter() {
            if !solution_state.solved {
                if print_missing_solutions {
                    error!(LOGGER, "no solution for block {:?}", solution_state.problem);
                }
                everything_solved = false;
            }
        }
        everything_solved
    }
}

/// This builds the solver chain:
/// - build `engine_sender`/`engine_receiver` pair to send engines to `Solver`
/// - add channel to `engine_sender` that will notify us of engine being exhausted
/// - make a channel to get solutions back
/// - build a solver and connect everything to it
fn build_solvers() -> (
    work::EngineSender,
    mpsc::UnboundedReceiver<hal::UniqueMiningWorkSolution>,
    mpsc::UnboundedReceiver<work::DynWorkEngine>,
    work::Solver,
) {
    let (reschedule_sender, reschedule_receiver) = mpsc::unbounded();
    let (engine_sender, engine_receiver) = work::engine_channel(Some(reschedule_sender));
    let (solution_queue_tx, solution_queue_rx) = mpsc::unbounded();
    (
        // Send engines here (preferably OneWork engines)
        engine_sender,
        // Receive solutions from this
        solution_queue_rx,
        // Receive exhausted engines here (once OneWorkEngine has been turned into MiningWork,
        // then you will be able to receive it here)
        reschedule_receiver,
        // This is a solver that you hand off to backend
        work::Solver::new(engine_receiver, solution_queue_tx),
    )
}

async fn collect_solutions(
    mut solution_queue_rx: mpsc::UnboundedReceiver<hal::UniqueMiningWorkSolution>,
    registry: Arc<Mutex<Registry>>,
) {
    while let Some(solution) = await!(solution_queue_rx.next()) {
        let job: &test_utils::TestBlock = solution.job();
        info!(
            LOGGER,
            "received: was={:08x} got={:08x} ms={} hash={}",
            job.nonce,
            solution.nonce(),
            solution.midstate_idx(),
            solution.hash()
        );
        await!(registry.lock())
            .expect("registry lock")
            .add_solution(solution.into());
    }
}

#[test]
fn test_block_mining() {
    // create shutdown channel
    let (shutdown_sender, shutdown_receiver) = hal::Shutdown::new().split();

    // this is a small miner core: we generate work, collect solutions, and we pair them together
    // we expect all (generated) problems to be solved
    utils::run_async_main_exits(async {
        // Create solver and channels to send/receive work
        let (mut engine_sender, solution_queue_rx, mut reschedule_receiver, work_solver) =
            build_solvers();

        // create mining stats
        let mining_stats = Arc::new(Mutex::new(hal::MiningStats::new()));

        // create problem registry
        let registry = Arc::new(Mutex::new(Registry::new()));

        // start HW backend for selected target
        hal::run(work_solver, mining_stats.clone(), shutdown_sender);

        // start task to collect solutions and put them to registry
        tokio::spawn(collect_solutions(solution_queue_rx, registry.clone()).compat_fix());

        // TODO: first work sent to miner is for some reason ignored
        // workaround: send two works
        engine_sender.broadcast(Arc::new(test_utils::OneWorkEngine::new(
            Problem::new((&test_utils::TEST_BLOCKS[0]).into(), 0).into(),
        )));

        // generate all blocks for all possible midstates
        for target_midstate in 0..config::MIDSTATE_COUNT {
            for test_block in test_utils::TEST_BLOCKS.iter() {
                let problem = Problem {
                    model_solution: test_block.into(),
                    target_midstate,
                };
                let is_unique = await!(registry.lock())
                    .expect("registry lock")
                    .add_problem(problem.clone());
                if !is_unique {
                    panic!("duplicate problem");
                }
                // wait for the work (engine) to be sent out (exhausted)
                await!(reschedule_receiver.next());
                engine_sender.broadcast(Arc::new(test_utils::OneWorkEngine::new(
                    problem.clone().into(),
                )));
            }
        }

        // wait for hw to finish computation
        let timeout_started = Instant::now();
        while timeout_started.elapsed() < config::JOB_TIMEOUT {
            await!(Delay::new(Instant::now() + Duration::from_secs(1))).unwrap();
            if await!(registry.lock())
                .expect("registry lock failed")
                .check_everything_solved(false)
            {
                break;
            }
        }

        // go through registry and check if everything was solved
        let registry = await!(registry.lock()).expect("registry lock");
        assert!(registry.check_everything_solved(true));
    });

    // the shutdown receiver has to survive up to this point to prevent shutdown sends by dying tasks to fail
    drop(shutdown_receiver);
}

#[test]
fn test_registry() {
    let mut registry = Registry::new();
    let block1: hal::UniqueMiningWorkSolution = (&test_utils::TEST_BLOCKS[0]).into();
    let block2: hal::UniqueMiningWorkSolution = (&test_utils::TEST_BLOCKS[1]).into();

    // problem can be inserted only once
    assert!(registry.add_problem(Problem::new(block1.clone(), 2)));
    assert!(!registry.add_problem(Problem::new(block1.clone(), 2)));
    // nothing is solved yet
    assert!(!registry.check_everything_solved(false));
    // solve everything and check
    registry.add_solution(Solution::new(block1.clone(), 2));
    assert!(registry.check_everything_solved(false));

    // re-inserting problem doesn't unsolve it
    assert!(!registry.add_problem(Problem::new(block1.clone(), 2)));
    assert!(registry.check_everything_solved(false));

    // test multiple problems
    assert!(registry.add_problem(Problem::new(block1.clone(), 1)));
    assert!(!registry.add_problem(Problem::new(block1.clone(), 1)));
    assert!(registry.add_problem(Problem::new(block2.clone(), 3)));
    assert!(!registry.check_everything_solved(false));
    registry.add_solution(Solution::new(block2.clone(), 3));
    assert!(!registry.check_everything_solved(false));
    registry.add_solution(Solution::new(block1.clone(), 1));
    assert!(registry.check_everything_solved(false));
}