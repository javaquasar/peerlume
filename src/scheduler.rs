use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiecePriority {
    Skip,
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PieceStrategy {
    #[default]
    RarestFirst,
    Sequential,
}

#[derive(Clone, Debug)]
struct PieceState {
    priority: PiecePriority,
    availability: u32,
    verified: bool,
    owners: Vec<String>,
}

pub struct PieceScheduler {
    pieces: Vec<PieceState>,
    peers: HashMap<String, Vec<bool>>,
    strategy: PieceStrategy,
    endgame_threshold: usize,
    endgame_replicas: usize,
}

impl PieceScheduler {
    pub fn new(piece_count: usize, strategy: PieceStrategy) -> Self {
        Self {
            pieces: vec![
                PieceState {
                    priority: PiecePriority::Normal,
                    availability: 0,
                    verified: false,
                    owners: Vec::new(),
                };
                piece_count
            ],
            peers: HashMap::new(),
            strategy,
            endgame_threshold: 8,
            endgame_replicas: 2,
        }
    }

    pub fn set_endgame(&mut self, threshold: usize, replicas: usize) -> Result<(), String> {
        if replicas == 0 {
            return Err("endgame replica count must be greater than zero".into());
        }
        self.endgame_threshold = threshold;
        self.endgame_replicas = replicas;
        Ok(())
    }

    pub fn set_priority(
        &mut self,
        piece_index: usize,
        priority: PiecePriority,
    ) -> Result<(), String> {
        let piece = self
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| "piece index is out of range".to_owned())?;
        piece.priority = priority;
        if priority == PiecePriority::Skip {
            piece.owners.clear();
        }
        Ok(())
    }

    pub fn update_peer(
        &mut self,
        peer_id: impl Into<String>,
        bitfield: Vec<bool>,
    ) -> Result<(), String> {
        if bitfield.len() != self.pieces.len() {
            return Err("peer bitfield length does not match piece count".into());
        }
        self.peers.insert(peer_id.into(), bitfield);
        self.recalculate_availability();
        Ok(())
    }

    pub fn remove_peer(&mut self, peer_id: &str) {
        self.peers.remove(peer_id);
        for piece in &mut self.pieces {
            piece.owners.retain(|owner| owner != peer_id);
        }
        self.recalculate_availability();
    }

    pub fn reserve_for_peer(&mut self, peer_id: &str) -> Option<u32> {
        let bitfield = self.peers.get(peer_id)?;
        let remaining = self
            .pieces
            .iter()
            .filter(|piece| piece.priority != PiecePriority::Skip && !piece.verified)
            .count();
        let replica_limit = if remaining <= self.endgame_threshold {
            self.endgame_replicas
        } else {
            1
        };
        let strategy = self.strategy;
        let candidate = self
            .pieces
            .iter()
            .enumerate()
            .filter(|(index, piece)| {
                bitfield[*index]
                    && piece.priority != PiecePriority::Skip
                    && !piece.verified
                    && piece.owners.len() < replica_limit
                    && !piece.owners.iter().any(|owner| owner == peer_id)
            })
            .min_by_key(|(index, piece)| {
                let priority_rank = PiecePriority::High as u8 - piece.priority as u8;
                match strategy {
                    PieceStrategy::RarestFirst => {
                        (priority_rank, piece.availability as usize, *index)
                    }
                    PieceStrategy::Sequential => {
                        (priority_rank, *index, piece.availability as usize)
                    }
                }
            })
            .map(|(index, _)| index)?;
        self.pieces[candidate].owners.push(peer_id.to_owned());
        Some(candidate as u32)
    }

    pub fn release(&mut self, piece_index: usize, peer_id: &str) -> Result<(), String> {
        let piece = self
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| "piece index is out of range".to_owned())?;
        piece.owners.retain(|owner| owner != peer_id);
        Ok(())
    }

    pub fn mark_verified(&mut self, piece_index: usize) -> Result<Vec<String>, String> {
        let piece = self
            .pieces
            .get_mut(piece_index)
            .ok_or_else(|| "piece index is out of range".to_owned())?;
        piece.verified = true;
        Ok(std::mem::take(&mut piece.owners))
    }

    pub fn remaining(&self) -> usize {
        self.pieces
            .iter()
            .filter(|piece| piece.priority != PiecePriority::Skip && !piece.verified)
            .count()
    }

    fn recalculate_availability(&mut self) {
        for (index, piece) in self.pieces.iter_mut().enumerate() {
            piece.availability = self
                .peers
                .values()
                .filter(|bitfield| bitfield[index])
                .count()
                .try_into()
                .unwrap_or(u32::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rarest_first_honors_priority_before_availability() {
        let mut scheduler = PieceScheduler::new(3, PieceStrategy::RarestFirst);
        scheduler.update_peer("a", vec![true, true, true]).unwrap();
        scheduler.update_peer("b", vec![true, false, true]).unwrap();
        assert_eq!(scheduler.reserve_for_peer("a"), Some(1));
        scheduler.set_priority(2, PiecePriority::High).unwrap();
        assert_eq!(scheduler.reserve_for_peer("b"), Some(2));
    }

    #[test]
    fn sequential_mode_skips_disabled_and_verified_pieces() {
        let mut scheduler = PieceScheduler::new(4, PieceStrategy::Sequential);
        scheduler.update_peer("peer", vec![true; 4]).unwrap();
        scheduler.set_priority(0, PiecePriority::Skip).unwrap();
        scheduler.mark_verified(1).unwrap();
        assert_eq!(scheduler.reserve_for_peer("peer"), Some(2));
    }

    #[test]
    fn endgame_allows_bounded_duplicate_requests() {
        let mut scheduler = PieceScheduler::new(1, PieceStrategy::RarestFirst);
        scheduler.set_endgame(1, 2).unwrap();
        scheduler.update_peer("a", vec![true]).unwrap();
        scheduler.update_peer("b", vec![true]).unwrap();
        scheduler.update_peer("c", vec![true]).unwrap();
        assert_eq!(scheduler.reserve_for_peer("a"), Some(0));
        assert_eq!(scheduler.reserve_for_peer("b"), Some(0));
        assert_eq!(scheduler.reserve_for_peer("c"), None);
        assert_eq!(scheduler.mark_verified(0).unwrap(), ["a", "b"]);
        assert_eq!(scheduler.remaining(), 0);
    }

    #[test]
    fn disconnected_peer_releases_ownership() {
        let mut scheduler = PieceScheduler::new(1, PieceStrategy::RarestFirst);
        scheduler.update_peer("a", vec![true]).unwrap();
        scheduler.update_peer("b", vec![true]).unwrap();
        assert_eq!(scheduler.reserve_for_peer("a"), Some(0));
        scheduler.remove_peer("a");
        assert_eq!(scheduler.reserve_for_peer("b"), Some(0));
    }
}
