use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Blacklist {
    failures: HashMap<IpAddr, (u32, Instant)>,
    bans: HashMap<IpAddr, Instant>,
    max_failures: u32,
    ban_duration: Duration,
    failure_window: Duration,
}

impl Blacklist {
    pub fn new(max_failures: u32, ban_duration: Duration, failure_window: Duration) -> Self {
        Self {
            failures: HashMap::new(),
            bans: HashMap::new(),
            max_failures,
            ban_duration,
            failure_window,
        }
    }

    pub fn is_banned(&mut self, ip: IpAddr) -> bool {
        if let Some(&ban_until) = self.bans.get(&ip) {
            if Instant::now() < ban_until {
                return true;
            } else {
                self.bans.remove(&ip);
            }
        }
        false
    }

    pub fn record_failure(&mut self, ip: IpAddr) {
        let now = Instant::now();

        // Check if already banned to avoid updating failure count unnecessarily
        if self.is_banned(ip) {
            return;
        }

        let entry = self.failures.entry(ip).or_insert((0, now));

        // Reset count if window expired
        if now.duration_since(entry.1) > self.failure_window {
            *entry = (1, now);
        } else {
            entry.0 += 1;
            entry.1 = now;
        }

        if entry.0 >= self.max_failures {
            self.bans.insert(ip, now + self.ban_duration);
            self.failures.remove(&ip);
            println!("Banned IP: {} for {:?}", ip, self.ban_duration);
        }
    }

    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.bans.retain(|_, &mut until| until > now);
        self.failures
            .retain(|_, &mut (_, last)| now.duration_since(last) < self.failure_window);
    }
}
