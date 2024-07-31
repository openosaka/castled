use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashSet;
use rand::prelude::*;
use rand::rngs::StdRng;
use rand::seq::IteratorRandom;

#[derive(Clone)]
pub struct PortManager {
    min: u16,
    max: u16,
    exclude_ports: Arc<DashSet<u16>>,
    /// TODO(sword): remove this mutex.
    /// we don't need it because we only has one actual consumer of PortManager
    /// but the caller code requires `Sync`, so we have to use `Mutex`.
    rng: Arc<Mutex<StdRng>>,
    pool: Arc<DashSet<u16>>,
}

impl PortManager {
    pub fn new(port_range: std::ops::RangeInclusive<u16>, exclude_ports: Vec<u16>) -> Self {
        let min = *port_range.start();
        let max = *port_range.end();
        let ports: Vec<_> = port_range
            .filter(|port| !exclude_ports.contains(port))
            .collect();

        let pool = DashSet::from_iter(ports);
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let seed = since_the_epoch.as_secs();
        let rng = StdRng::seed_from_u64(seed);
        Self {
            rng: Arc::new(Mutex::new(rng)),
            min,
            max,
            exclude_ports: Arc::new(exclude_ports.into_iter().collect()),
            pool: Arc::new(pool),
        }
    }

    // check if the port is a valid port, doesn't guarantee the port is available.
    pub fn allow(&self, port: u16) -> bool {
        port >= self.min && port <= self.max && !self.exclude_ports.contains(&port)
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
        let mut rng = self.rng.lock().unwrap();
        let mut rng = &mut *rng;
        let port = *self.pool.iter().choose(&mut rng)?;
        println!("get port: {}", port);
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
    use std::{collections::HashSet, ops::RangeInclusive};

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

    #[test]
    fn test_it_shares_same_memory() {
        let port_range: RangeInclusive<u16> = 3000..=3011;
        let exclude_ports = vec![3010];
        let len = port_range.len() - exclude_ports.len();
        let mut port_manager = PortManager::new(port_range, exclude_ports.clone());
        let mut port_manager2 = port_manager.clone();

        let _p1 = port_manager.get();
        let _p2 = port_manager.get();
        let _p3 = port_manager.get();
        let _p4 = port_manager2.get();
        let _p5 = port_manager2.get();

        assert_eq!(port_manager.pool.len(), len - 5);
        assert_eq!(port_manager.pool.len(), port_manager2.pool.len());
        for port in port_manager.pool.iter() {
            assert!(port_manager2.pool.contains(&port));
        }

        let mut ports = vec![];
        for _ in 0..100 {
            let mut port_manager = port_manager.clone();
            let available_port = port_manager.get().unwrap();
            let port = *available_port;
            ports.push(port);
        }
        assert!(
            // test randomness
            ports
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .len()
                > 1
        );

        port_manager.get();
        port_manager.get();
        port_manager.get();
        port_manager.get();
        port_manager.get();
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
