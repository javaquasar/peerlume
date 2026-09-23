use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

struct PoolState {
    idle: Vec<Vec<u8>>,
    allocated: usize,
}

#[derive(Clone)]
pub struct BlockBufferPool {
    block_bytes: usize,
    max_buffers: usize,
    state: Arc<Mutex<PoolState>>,
}

pub struct BlockBuffer {
    bytes: Option<Vec<u8>>,
    pool: BlockBufferPool,
}

impl BlockBufferPool {
    pub fn new(block_bytes: usize, max_buffers: usize) -> Result<Self, String> {
        if block_bytes == 0 || max_buffers == 0 {
            return Err("buffer size and count must be greater than zero".into());
        }
        Ok(Self {
            block_bytes,
            max_buffers,
            state: Arc::new(Mutex::new(PoolState {
                idle: Vec::new(),
                allocated: 0,
            })),
        })
    }

    pub fn acquire(&self) -> Option<BlockBuffer> {
        let mut state = self.state.lock().ok()?;
        let bytes = if let Some(bytes) = state.idle.pop() {
            bytes
        } else if state.allocated < self.max_buffers {
            state.allocated += 1;
            vec![0; self.block_bytes]
        } else {
            return None;
        };
        Some(BlockBuffer {
            bytes: Some(bytes),
            pool: self.clone(),
        })
    }

    pub fn allocated_bytes(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.allocated.saturating_mul(self.block_bytes))
            .unwrap_or(0)
    }
}

impl Deref for BlockBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes
            .as_deref()
            .expect("block buffer owns bytes until drop")
    }
}

impl DerefMut for BlockBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bytes
            .as_deref_mut()
            .expect("block buffer owns bytes until drop")
    }
}

impl Drop for BlockBuffer {
    fn drop(&mut self) {
        let Some(mut bytes) = self.bytes.take() else {
            return;
        };
        bytes.fill(0);
        if let Ok(mut state) = self.pool.state.lock() {
            state.idle.push(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_bounds_allocations_and_reuses_zeroed_blocks() {
        let pool = BlockBufferPool::new(16, 2).unwrap();
        let mut first = pool.acquire().unwrap();
        first[0] = 99;
        let second = pool.acquire().unwrap();
        assert!(pool.acquire().is_none());
        assert_eq!(pool.allocated_bytes(), 32);
        drop(first);
        let reused = pool.acquire().unwrap();
        assert_eq!(reused[0], 0);
        drop(second);
        drop(reused);
        assert_eq!(pool.allocated_bytes(), 32);
    }
}
