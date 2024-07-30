use std::sync::Arc;

use dashmap::DashSet;
use rand::prelude::*;
use rand::seq::IteratorRandom;
use rand_chacha::ChaCha8Rng;

#[derive(Clone)]
pub struct PortManager {
    rng: ChaCha8Rng,
    pool: Arc<DashSet<u16>>,
}

impl PortManager {
    pub fn new(port_range: std::ops::RangeInclusive<u16>, exclude_ports: Vec<u16>) -> Self {
        let ports: Vec<_> = port_range
            .filter(|port| !exclude_ports.contains(port))
            .collect();

        let pool = DashSet::from_iter(ports);
        let rng = ChaCha8Rng::from_entropy();
        Self {
            rng,
            pool: Arc::new(pool),
        }
    }

    pub fn has(&self, port: u16) -> bool {
        self.pool.contains(&port)
    }

    // take a port from the pool.
    //
    // Returns
    // - Some(Available) if the port is available.
    // - None if the port is not in the pool.
    pub fn take(&self, port: u16) -> Option<Available> {
        if !self.pool.contains(&port) {
            return None;
        }
        self.pool.remove(&port);

        Some(Available {
            port,
            pool: Arc::clone(&self.pool),
            unavailable: false,
        })
    }

    // get a random available port.
    pub fn get(&mut self) -> Option<Available> {
        let port = *self.pool.iter().choose(&mut self.rng)?;
        self.take(port)
    }
}

#[derive(Debug)]
pub struct Available {
    /// the port number.
    port: u16,
    /// the pool may not exist if creates `Available` directly.
    pool: Arc<DashSet<u16>>,
    /// when bind fails, set this to true.
    /// we don't return the port to the pool when it's unavailable.
    unavailable: bool,
}

impl Available {
    pub(crate) fn unavailable(&mut self) {
        self.unavailable = true;
    }
}

impl Drop for Available {
    fn drop(&mut self) {
        if self.unavailable {
            return;
        }
        self.pool.insert(self.port);
    }
}

impl std::ops::Deref for Available {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        &self.port
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
    }

    #[tokio::test]
    async fn test_clone_and_parallel() {
        let port_range: RangeInclusive<u16> = 3000..=5000;
        let exclude_ports = vec![3010];
        let len = port_range.len() - exclude_ports.len();
        let port_manager = PortManager::new(port_range, exclude_ports.clone());

        let (port_tx, mut port_rx) = tokio::sync::mpsc::channel(10);
        for _ in 0..10 {
            let mut port_manager = port_manager.clone();
            let port_tx = port_tx.clone();
            tokio::spawn(async move {
                loop {
                    let port = port_manager.get();
                    if port.is_none() {
                        break;
                    }
                    port_tx.send(port).await.unwrap();
                }
            });
        }

        let mut ports = Vec::new();
        while let Some(port) = port_rx.recv().await {
            ports.push(*port.unwrap());
            if ports.len() == len {
                break;
            }
        }
        assert_eq!(ports.len(), len);
        assert!(!ports.contains(&3010));
    }
}
