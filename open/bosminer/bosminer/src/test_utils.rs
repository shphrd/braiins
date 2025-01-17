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

pub mod block_mining;

use crate::job;
use crate::work;

pub use ii_bitcoin::{TestBlock, TEST_BLOCKS};

use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard};

impl job::Bitcoin for TestBlock {
    fn version(&self) -> u32 {
        self.version
    }

    fn version_mask(&self) -> u32 {
        0
    }

    fn previous_hash(&self) -> &ii_bitcoin::DHash {
        &self.previous_hash
    }

    fn merkle_root(&self) -> &ii_bitcoin::DHash {
        &self.merkle_root
    }

    fn time(&self) -> u32 {
        self.time
    }

    fn bits(&self) -> u32 {
        self.bits
    }

    fn is_valid(&self) -> bool {
        true
    }
}

/// WorkEngine for testing purposes that carries exactly one piece of `MiningWork`
#[derive(Debug)]
struct OneWorkEngineInner {
    work: Option<work::Assignment>,
}

impl OneWorkEngineInner {
    fn is_exhausted(&self) -> bool {
        self.work.is_none()
    }

    fn next_work(&mut self) -> work::LoopState<work::Assignment> {
        match self.work.take() {
            Some(work) => work::LoopState::Break(work),
            None => work::LoopState::Exhausted,
        }
    }
}

/// Wrapper for `OneWorkEngineInner` to allow shared access.
#[derive(Debug)]
pub struct OneWorkEngine {
    /// Standard Mutex allows create `TestWorkEngineInner` with mutable self reference in
    /// `next_work` and it also satisfies `hal::WorkEngine` requirement for `Send + Sync`
    inner: StdMutex<OneWorkEngineInner>,
}

impl OneWorkEngine {
    pub fn new(work: work::Assignment) -> Self {
        Self {
            inner: StdMutex::new(OneWorkEngineInner { work: Some(work) }),
        }
    }

    fn lock_inner(&self) -> StdMutexGuard<OneWorkEngineInner> {
        self.inner.lock().expect("cannot lock test work engine")
    }
}

impl work::Engine for OneWorkEngine {
    fn is_exhausted(&self) -> bool {
        self.lock_inner().is_exhausted()
    }

    fn next_work(&self) -> work::LoopState<work::Assignment> {
        self.lock_inner().next_work()
    }
}

#[derive(Debug)]
struct TestWorkEngineInner {
    next_test_block: Option<&'static TestBlock>,
    test_block_iter: std::slice::Iter<'static, TestBlock>,
}

impl TestWorkEngineInner {
    fn is_exhausted(&self) -> bool {
        self.next_test_block.is_none()
    }

    fn next_work(&mut self) -> work::LoopState<work::Assignment> {
        if self.is_exhausted() {
            return work::LoopState::Exhausted;
        }

        match self.test_block_iter.next() {
            None => work::LoopState::Break(self.next_test_block.take()),
            Some(block) => work::LoopState::Continue(self.next_test_block.replace(block)),
        }
        .map(|block| block.expect("test block is 'None'").into())
    }
}

#[derive(Debug)]
pub struct TestWorkEngine {
    /// Standard Mutex allows create `TestWorkEngineInner` with mutable self reference in
    /// `next_work` and it also satisfies `hal::WorkEngine` requirement for `Send + Sync`
    inner: StdMutex<TestWorkEngineInner>,
}

impl TestWorkEngine {
    pub fn new() -> Self {
        let mut test_block_iter = TEST_BLOCKS.iter();
        let next_test_block = test_block_iter.next();

        Self {
            inner: StdMutex::new(TestWorkEngineInner {
                next_test_block,
                test_block_iter,
            }),
        }
    }

    fn lock_inner(&self) -> StdMutexGuard<TestWorkEngineInner> {
        self.inner.lock().expect("cannot lock test work engine")
    }
}

impl work::Engine for TestWorkEngine {
    fn is_exhausted(&self) -> bool {
        self.lock_inner().is_exhausted()
    }

    fn next_work(&self) -> work::LoopState<work::Assignment> {
        self.lock_inner().next_work()
    }
}

pub fn create_test_work_receiver() -> work::EngineReceiver {
    let work_engine = Arc::new(TestWorkEngine::new());
    let (mut sender, receiver) = work::engine_channel(None);
    sender.broadcast(work_engine);
    receiver
}

pub fn create_test_work_generator() -> work::Generator {
    work::Generator::new(create_test_work_receiver())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::job::Bitcoin;

    fn get_engine(work_receiver: &mut work::EngineReceiver) -> Arc<work::Engine> {
        ii_async_compat::block_on(work_receiver.get_engine()).expect("cannot get test work engine")
    }

    fn cmp_block_with_work(block: &TestBlock, work: work::Assignment) -> work::Assignment {
        assert_eq!(block.midstate, work.midstates[0].state);
        assert_eq!(block.merkle_root_tail(), work.merkle_root_tail());
        assert_eq!(block.time(), work.ntime);
        assert_eq!(block.bits(), work.bits());
        work
    }

    #[test]
    fn test_work_receiver() {
        let mut work_receiver = create_test_work_receiver();
        let test_engine = get_engine(&mut work_receiver);

        // test work engine is not exhausted so it should return the same engine
        assert!(Arc::ptr_eq(&test_engine, &get_engine(&mut work_receiver)));

        let mut work_break = false;
        for block in TEST_BLOCKS.iter() {
            match test_engine
                .next_work()
                .map(|work| cmp_block_with_work(block, work))
            {
                work::LoopState::Exhausted => {
                    panic!("test work generator returned less work than expected")
                }
                work::LoopState::Break(_) => {
                    assert!(!work_break, "test work generator returned double break");
                    work_break = true;
                }
                work::LoopState::Continue(_) => {
                    assert!(!work_break, "test work generator continues after break")
                }
            }
        }
        assert!(
            work_break,
            "test work generator returned more work than expected"
        );
        match test_engine.next_work() {
            work::LoopState::Exhausted => (),
            _ => panic!("test work generator continues after returning all work"),
        };
    }
}
