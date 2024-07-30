use std::sync::Arc;

use dashmap::DashSet;
use rand::prelude::*;
use rand::seq::IteratorRandom;
use rand_chacha::ChaCha20Rng;

#[derive(Clone)]
pub struct PortManager {
    rng: ChaCha20Rng,
    pool: Arc<DashSet<u16>>,
}

impl PortManager {
    pub fn new(port_range: std::ops::RangeInclusive<u16>, exclude_ports: Vec<u16>) -> Self {
        let ports: Vec<_> = port_range
            .filter(|port| !exclude_ports.contains(port))
            .collect();

        let pool = DashSet::from_iter(ports);
        let rng = ChaCha20Rng::from_entropy();
        Self {
            rng,
            pool: Arc::new(pool),
        }
    }

    pub fn get(&mut self) -> Option<Available> {
        let port = *self.pool.iter().choose(&mut self.rng)?;
        self.pool.remove(&port);
        Some(Available {
            port,
            pool: Some(Arc::clone(&self.pool)),
        })
    }

    pub fn remove(&mut self, port: u16) {
        self.pool.remove(&port);
    }
}

#[derive(Debug)]
pub struct Available {
    port: u16,
    /// the pool may not exist if creates `Available` directly.
    pool: Option<Arc<DashSet<u16>>>,
}

impl Drop for Available {
    fn drop(&mut self) {
        if let Some(pool) = &self.pool {
            pool.insert(self.port);
        }
    }
}

impl std::ops::Deref for Available {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.port
    }
}

impl From<u16> for Available {
    fn from(port: u16) -> Self {
        Self { port, pool: None }
    }
}

impl From<Available> for u16 {
    fn from(available: Available) -> Self {
        available.port
    }
}

#[cfg(test)]
mod test {
    use std::ops::RangeInclusive;

    use super::*;

    #[test]
    fn test_port_manager() {
        let port_range: RangeInclusive<u16> = 2000..=2100;
        let exclude_ports = vec![2010, 2020];
        let len = port_range.len() - exclude_ports.len();
        let mut port_manager = PortManager::new(port_range, exclude_ports.clone());

        for _ in 0..10000 {
            let port = port_manager.get();
            assert!(port.is_some());
            // auto drop
        }

        let ports: DashSet<u16> = Default::default();
        for _ in 0..1000 {
            let port = port_manager.get();
            if port.is_none() {
                break;
            }
            ports.insert(*port.unwrap());
        }
        assert_eq!(ports.len(), len);
        assert!(!ports.contains(&2010));
        assert!(!ports.contains(&2020));

        drop(ports);

        port_manager.remove(2000);
        let ports: DashSet<u16> = Default::default();
        for _ in 0..1000 {
            let port = port_manager.get();
            if port.is_none() {
                break;
            }
            ports.insert(*port.unwrap());
        }
        assert_eq!(ports.len(), len - 1);
        assert!(!ports.contains(&2010));
        assert!(!ports.contains(&2020));
        assert!(!ports.contains(&2000));
    }
}
