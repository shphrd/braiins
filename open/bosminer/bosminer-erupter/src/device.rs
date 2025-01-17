// Copyright (C) 2019  Braiins Systems s.r.o.
//
// This file is part of Braiins Open-Source Initiative (BOSI).
//
// BOSI is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Please, keep in mind that we may also license BOSI or any part thereof
// under a proprietary license. For more information on the terms and conditions
// of such proprietary license or if you have any other questions, please
// contact us at opensource@braiins.com.

//! Provides Block Erupter USB driver witch translates work generated by `work::Generator` into
//! a form that is recognized by the hashing chip

use super::error::{self, ErrorKind};
use super::icarus;

use bosminer::work;

use failure::{Fail, ResultExt};

use std::cell::RefCell;
use std::convert::TryInto;
use std::mem::size_of;
use std::time::{Duration, SystemTime};

const CP210X_TYPE_OUT: u8 = 0x41;
const CP210X_REQUEST_IFC_ENABLE: u8 = 0x00;
const CP210X_REQUEST_DATA: u8 = 0x07;
const CP210X_REQUEST_BAUD: u8 = 0x1e;

const CP210X_VALUE_UART_ENABLE: u16 = 0x0001;
const CP210X_VALUE_DATA: u16 = 0x0303;
const CP210X_DATA_BAUD: u32 = 115200;

const ID_VENDOR: u16 = 0x10c4;
const ID_PRODUCT: u16 = 0xea60;

const DEVICE_IFACE: u8 = 0;
const DEVICE_CONFIGURATION: u8 = 1;

const WRITE_ADDR: u8 = 0x1;
const READ_ADDR: u8 = 0x81;

// propagation delay of USB device
const WAIT_TIMEOUT_MS: u64 = 100;
const WAIT_TIMEOUT: Duration = Duration::from_millis(WAIT_TIMEOUT_MS);

/// How many ms below the expected completion time to abort work
/// extra in case the last read is delayed
const READ_REDUCE_MS: f64 = WAIT_TIMEOUT_MS as f64 * 1.5;

/// Timeout in ms for reading nonce from USB -> UART bridge read
/// initialization has some latency which is reduced from full nonce time
const MAX_READ_TIME: Duration =
    Duration::from_millis((icarus::FULL_NONCE_TIME_MS - READ_REDUCE_MS) as u64);

pub struct BlockErupter<'a> {
    context: &'a libusb::Context,
    device: libusb::DeviceHandle<'a>,
}

impl<'a> BlockErupter<'a> {
    pub fn new(context: &'a libusb::Context, device: libusb::DeviceHandle<'a>) -> Self {
        Self { context, device }
    }

    /// Try to find Block Erupter connected to USB
    /// Only first device is returned when multiple Block Erupters are connected.
    pub fn find(context: &'a libusb::Context) -> Option<Self> {
        context
            .open_device_with_vid_pid(ID_VENDOR, ID_PRODUCT)
            .map(|device| Self::new(context, device))
    }

    /// Initialize Block Erupter device to accept work to solution
    /// The USB device using a standard `CP210x` chip, which results in loading standard driver into
    /// the kernel for handling USB to UART bridge. This initialization tries to detach this driver
    /// from the kernel and provide its own implementation implemented by the `libusb` library.
    pub fn init(&mut self) -> error::Result<()> {
        self.device
            .reset()
            .with_context(|_| ErrorKind::Usb("cannot reset device"))?;

        if self.context.supports_detach_kernel_driver() {
            if self
                .device
                .kernel_driver_active(DEVICE_IFACE)
                .with_context(|_| ErrorKind::Usb("cannot detect kernel driver"))?
            {
                self.device
                    .detach_kernel_driver(DEVICE_IFACE)
                    .with_context(|_| ErrorKind::Usb("cannot detach kernel driver"))?;
            }
        }

        self.device
            .set_active_configuration(DEVICE_CONFIGURATION)
            .with_context(|_| ErrorKind::Usb("cannot set active configuration"))?;

        // enable the UART
        self.device
            .write_control(
                CP210X_TYPE_OUT,
                CP210X_REQUEST_IFC_ENABLE,
                CP210X_VALUE_UART_ENABLE,
                0,
                &[],
                WAIT_TIMEOUT,
            )
            .with_context(|_| ErrorKind::Usb("cannot enable UART"))?;
        // set data control
        self.device
            .write_control(
                CP210X_TYPE_OUT,
                CP210X_REQUEST_DATA,
                CP210X_VALUE_DATA,
                0,
                &[],
                WAIT_TIMEOUT,
            )
            .with_context(|_| ErrorKind::Usb("cannot set data control"))?;
        // set the baud
        self.device
            .write_control(
                CP210X_TYPE_OUT,
                CP210X_REQUEST_BAUD,
                0,
                0,
                &CP210X_DATA_BAUD.to_le_bytes(),
                WAIT_TIMEOUT,
            )
            .with_context(|_| ErrorKind::Usb("cannot set baud rate"))?;

        Ok(())
    }

    /// Send new work to the device
    /// All old work is interrupted immediately and the search space is restarted for the new work.  
    pub fn send_work(&self, work: icarus::WorkPayload) -> error::Result<()> {
        self.device
            .write_bulk(WRITE_ADDR, &work.into_bytes(), WAIT_TIMEOUT)
            .with_context(|_| ErrorKind::Usb("cannot send work"))?;

        Ok(())
    }

    /// Wait for specified amount of time to find the nonce for current work
    /// The work have to be previously send using `send_work` method.
    /// More solution may exist so this method must be called multiple times to get all of them.
    /// When all search space is exhausted then the chip stops finding new nonce. The maximal time
    /// of searching is constant for this chip and after this time no new solution is found.
    /// The `None` is returned then timeout occurs and any nonce is found.
    /// It is possible that during sending new work the nonce for old one can be found and returned
    /// from this method!
    pub fn wait_for_nonce(&self, timeout: Duration) -> error::Result<Option<u32>> {
        let mut nonce = [0u8; size_of::<u32>()];
        match self.device.read_bulk(READ_ADDR, &mut nonce, timeout) {
            Ok(n) => {
                if n != size_of::<u32>() {
                    Err(ErrorKind::Usb("read incorrect number of bytes"))?
                };
                Ok(u32::from_le_bytes(nonce)
                    .try_into()
                    .expect("slice with incorrect length"))
            }
            Err(libusb::Error::Timeout) => Ok(None),
            Err(e) => Err(e.context(ErrorKind::Usb("cannot read nonce")).into()),
        }
    }

    /// Converts Block Erupter device into iterator which solving generated work
    pub fn into_solver(self, work_generator: work::Generator) -> BlockErupterSolver<'a> {
        BlockErupterSolver::new(self, work_generator)
    }
}

/// Wrap the Block Erupter device and work generator to implement iterable object which solves
/// incoming work and tries to find solution which is returned as an unique mining work solution
pub struct BlockErupterSolver<'a> {
    device: BlockErupter<'a>,
    work_generator: work::Generator,
    work_start: SystemTime,
    curr_work: Option<work::Assignment>,
    next_solution: Option<work::UniqueSolution>,
    solution_id: u32,
    stop_reason: RefCell<error::Result<()>>,
}

impl<'a> BlockErupterSolver<'a> {
    fn new(device: BlockErupter<'a>, work_generator: work::Generator) -> Self {
        Self {
            device,
            work_generator,
            work_start: SystemTime::UNIX_EPOCH,
            curr_work: None,
            next_solution: None,
            solution_id: 0,
            stop_reason: RefCell::new(Ok(())),
        }
    }

    /// Consume the iterator and return the reason of stream termination
    pub fn get_stop_reason(self) -> error::Result<()> {
        // the object is consumed so replacing with `Ok` is fine
        self.stop_reason.replace(Ok(()))
    }

    fn send_work(&mut self, work: &work::Assignment) {
        let work_payload = icarus::WorkPayload::new(
            &work.midstates[0].state,
            work.merkle_root_tail(),
            work.ntime,
            work.bits(),
        );
        self.work_start = SystemTime::now();
        self.device.send_work(work_payload).unwrap_or_else(|e| {
            *self.stop_reason.get_mut() = Err(e);
        });
    }

    fn wait_for_nonce(&self) -> Option<(u32, SystemTime)> {
        let duration = match SystemTime::now().duration_since(self.work_start) {
            Ok(value) => value,
            Err(e) => {
                *self.stop_reason.borrow_mut() = Err(e
                    .context(ErrorKind::Timer(
                        "cannot measure elapsed time of work solution",
                    ))
                    .into());
                return None;
            }
        };
        let timeout_rem = MAX_READ_TIME.checked_sub(duration).unwrap_or(WAIT_TIMEOUT);

        self.device
            .wait_for_nonce(timeout_rem)
            .unwrap_or_else(|e| {
                // return `None` to indicate that nonce wasn't found and store error to the object
                // the stop reason can be later obtained with `get_stop_reason`
                *self.stop_reason.borrow_mut() = Err(e);
                None
            })
            .map(|nonce| (nonce, SystemTime::now()))
    }

    fn create_unique_solution(
        work: work::Assignment,
        nonce: u32,
        timestamp: SystemTime,
        solution_id: u32,
    ) -> work::UniqueSolution {
        work::UniqueSolution::new(
            work,
            work::Solution {
                nonce,
                ntime: None,
                midstate_idx: 0,
                solution_idx: 0,
                solution_id,
            },
            Some(timestamp),
        )
    }
}

impl<'a> Iterator for BlockErupterSolver<'a> {
    type Item = work::UniqueSolution;

    /// Waits for new work and send it to the Block Erupter device
    /// When the solution is found then the result is returned as an unique mining work solution.
    /// When an error occurs then `None` is returned and the failure reason can be obtained with
    /// `get_stop_reason` method which consumes the iterator.
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(solution) = self.next_solution.take() {
            // return solution for new work
            // this solves the issue when the solution is found for old work during sending new one
            // the work validation determines if the nonce is solution for old work or new one
            return Some(solution);
        }
        let mut prev_work = None;
        while self.stop_reason.borrow().is_ok() {
            if let Some(work) = &self.curr_work {
                // waiting for solution for maximal remaining time
                if let Some((nonce, timestamp)) = self.wait_for_nonce() {
                    // found solution!
                    let solution = Self::create_unique_solution(
                        work.clone(),
                        nonce,
                        timestamp,
                        self.solution_id,
                    );
                    // increment counter for next solution id
                    self.solution_id = self.solution_id.checked_add(1).expect("too much solutions");
                    return Some(match prev_work.take() {
                        None => solution,
                        // when solution has been found very quickly then it is possible that the
                        // nonce corresponds to previous work
                        Some((prev_work, prev_solution_id)) => {
                            self.next_solution = Some(solution);
                            Self::create_unique_solution(
                                prev_work,
                                nonce,
                                timestamp,
                                prev_solution_id,
                            )
                        }
                    });
                }
                if self.stop_reason.borrow().is_err() {
                    // some error occurs during waiting for solution
                    break;
                }
            }

            prev_work = self.curr_work.take().map(|work| (work, self.solution_id));
            match ii_async_compat::block_on(self.work_generator.generate()) {
                // end of stream
                None => break,
                // send new work and wait for result in the next iteration when no error occurs
                Some(work) => {
                    self.send_work(&work);
                    self.curr_work = Some(work);
                    self.solution_id = 0;
                }
            };
        }
        // some error occurs or stream from work generator is closed
        None
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use bosminer::job::Bitcoin;
    use bosminer::test_utils;

    use std::ops::{Deref, DerefMut};
    use std::sync;

    use lazy_static::lazy_static;

    lazy_static! {
        pub static ref USB_CONTEXT_MUTEX: sync::Mutex<()> = sync::Mutex::new(());
        pub static ref USB_CONTEXT: libusb::Context =
            libusb::Context::new().expect("cannot create new USB context");
    }

    struct BlockErupterGuard<'a> {
        device: BlockErupter<'a>,
        // context guard have to be dropped after block erupter device
        // do not change the order of members!
        context_guard: sync::MutexGuard<'a, ()>,
    }

    impl<'a> BlockErupterGuard<'a> {
        fn new(device: BlockErupter<'a>, context_guard: sync::MutexGuard<'a, ()>) -> Self {
            Self {
                device,
                context_guard,
            }
        }

        fn into_device(self) -> (BlockErupter<'a>, sync::MutexGuard<'a, ()>) {
            (self.device, self.context_guard)
        }
    }

    impl<'a> Deref for BlockErupterGuard<'a> {
        type Target = BlockErupter<'a>;

        fn deref(&self) -> &Self::Target {
            &self.device
        }
    }

    impl<'a> DerefMut for BlockErupterGuard<'a> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.device
        }
    }

    /// Synchronization function to get only one device at one moment to allow parallel tests
    fn get_block_erupter<'a>() -> BlockErupterGuard<'a> {
        // lock USB context for mutual exclusion
        let mut context_guard = Some(USB_CONTEXT_MUTEX.lock().expect("cannot lock USB context"));

        let mut device = BlockErupter::find(&*USB_CONTEXT).unwrap_or_else(|| {
            // unlock the guard before panicking the thread!
            context_guard.take();
            panic!("cannot find Block Erupter device")
        });
        // try to initialize Block Erupter
        device.init().unwrap_or_else(|_| {
            context_guard.take();
            panic!("Block Erupter initialization failed")
        });

        // the USB context will be unlocked at the end of a test using this device
        BlockErupterGuard::new(device, context_guard.unwrap())
    }

    #[test]
    fn test_block_erupter_init() {
        let _device = get_block_erupter();
    }

    #[test]
    fn test_block_erupter_io() {
        let device = get_block_erupter();

        for (i, block) in test_utils::TEST_BLOCKS.iter().enumerate() {
            let work = icarus::WorkPayload::new(
                &block.midstate,
                block.merkle_root_tail(),
                block.time(),
                block.bits(),
            );

            // send new work generated from test block
            device
                .send_work(work)
                .expect("cannot send work to Block Erupter");

            // wait for solution
            let timeout = MAX_READ_TIME;
            let mut timeout_rem = timeout;
            let mut nonce_found = false;

            let start = SystemTime::now();
            loop {
                match device
                    .wait_for_nonce(timeout_rem)
                    .expect("cannot read nonce from Block Erupter")
                {
                    None => break,
                    Some(nonce) => {
                        if block.nonce == nonce {
                            nonce_found = true;
                            break;
                        }
                    }
                }
                let duration = SystemTime::now()
                    .duration_since(start)
                    .expect("SystemTime::duration_since failed");
                timeout_rem = timeout
                    .checked_sub(duration)
                    .unwrap_or(Duration::from_millis(1));
            }

            assert!(nonce_found, "solution for block {} cannot be found", i);
        }
    }

    #[test]
    fn test_block_erupter_solver() {
        let work_generator = test_utils::create_test_work_generator();
        let (device, _device_guard) = get_block_erupter().into_device();

        // convert Block Erupter device to work solver
        // the work is generated from test work generator
        let mut solver = device.into_solver(work_generator);

        let mut blocks_iter = test_utils::TEST_BLOCKS.iter();
        let mut block = blocks_iter.next().expect("there is no test block");

        for solution in &mut solver {
            if block.hash == solution.hash() {
                // when solution has been found for current block then
                // move to the next one and wait for its solution
                block = match blocks_iter.next() {
                    // stop finding solution when all test blocks are solved
                    None => break,
                    Some(value) => value,
                };
            }
        }
        solver.get_stop_reason().expect("solver failed");
        assert!(
            blocks_iter.next().is_none(),
            "Block Erupter solver does not solve all test blocks"
        );
    }
}
