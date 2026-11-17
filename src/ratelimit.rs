//! Rate limiting: `"600/min"` parsing and a monotonic token bucket.

use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// A rate limit specification, e.g. `600/min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateSpec {
    /// Number of allowed events per window.
    pub limit: u32,
    /// Window length.
    pub window: Duration,
}

impl RateSpec {
    /// Events per second, for refilling.
    pub fn per_second(&self) -> f64 {
        self.limit as f64 / self.window.as_secs_f64().max(0.001)
    }
}

impl FromStr for RateSpec {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Err(Error::Config("empty rate limit".into()));
        }
        let (count, unit) = s
            .split_once('/')
            .ok_or_else(|| Error::Config(format!("invalid rate `{s}`, expected e.g. `600/min`")))?;
        let limit: u32 = count
            .trim()
            .parse()
            .map_err(|_| Error::Config(format!("invalid rate count in `{s}`")))?;
        if limit == 0 {
            return Err(Error::Config(format!("rate limit must be > 0 in `{s}`")));
        }
        let secs = match unit.trim().to_ascii_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86400,
            other => {
                return Err(Error::Config(format!(
                    "unknown rate unit `{other}` in `{s}` (use s/min/h/d"
                )))
            }
        };
        if secs > Duration::MAX.as_secs() {
            return Err(Error::Config(format!("rate window too large in `{s}`")));
        }
        Ok(RateSpec {
            limit,
            window: Duration::from_secs(secs),
        })
    }
}

impl fmt::Display for RateSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.window.as_secs();
        let unit = match secs {
            1 => "s",
            60 => "min",
            3600 => "h",
            86400 => "d",
            _ => return write!(f, "{}/{}s", self.limit, secs),
        };
        write!(f, "{}/{}", self.limit, unit)
    }
}

/// A monotonic token bucket.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: Instant,
    spec: RateSpec,
}

impl TokenBucket {
    /// Create a full bucket.
    pub fn new(spec: RateSpec) -> Self {
        TokenBucket {
            capacity: spec.limit as f64,
            tokens: spec.limit as f64,
            refill_per_sec: spec.per_second(),
            last_refill: Instant::now(),
            spec,
        }
    }

    fn refill_at(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let added = elapsed.as_secs_f64() * self.refill_per_sec;
        if added > 0.0 {
            self.tokens = (self.tokens + added).min(self.capacity);
            self.last_refill = now;
        }
    }

    /// Try to take one token.
    pub fn allow_at(&mut self, now: Instant) -> bool {
        self.refill_at(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// How long until one token becomes available.
    pub fn retry_after_at(&mut self, now: Instant) -> Duration {
        self.refill_at(now);
        let missing = 1.0 - self.tokens;
        if missing <= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(missing / self.refill_per_sec.max(f64::EPSILON))
        }
    }

    /// How long until the bucket is completely full.
    pub fn time_to_full_at(&mut self, now: Instant) -> Duration {
        self.refill_at(now);
        let missing = self.capacity - self.tokens;
        Duration::from_secs_f64(missing.max(0.0) / self.refill_per_sec.max(f64::EPSILON))
    }

    /// The spec this bucket was built from.
    pub fn spec(&self) -> RateSpec {
        self.spec
    }
}

/// Thread-safe token bucket for a single API key.
#[derive(Debug)]
pub struct RateLimiter {
    bucket: Mutex<TokenBucket>,
}

impl RateLimiter {
    /// Create a limiter.
    pub fn new(spec: RateSpec) -> Self {
        RateLimiter {
            bucket: Mutex::new(TokenBucket::new(spec)),
        }
    }

    /// Try to acquire a token; returns retry-after when limited.
    pub fn check(&self) -> std::result::Result<(), Duration> {
        let now = Instant::now();
        let mut bucket = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
        if bucket.allow_at(now) {
            Ok(())
        } else {
            Err(bucket.retry_after_at(now))
        }
    }

    /// Specification for logging / status output.
    pub fn spec(&self) -> RateSpec {
        self.bucket.lock().unwrap_or_else(|e| e.into_inner()).spec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        assert_eq!(
            "600/min".parse::<RateSpec>().unwrap(),
            RateSpec {
                limit: 600,
                window: Duration::from_secs(60)
            }
        );
        assert_eq!("10/s".parse::<RateSpec>().unwrap().limit, 10);
        assert_eq!("5/hour".parse::<RateSpec>().unwrap().window.as_secs(), 3600);
        assert!("600".parse::<RateSpec>().is_err());
        assert!("0/s".parse::<RateSpec>().is_err());
        assert!("600/minx".parse::<RateSpec>().is_err());
    }

    #[test]
    fn bucket_allows_up_to_capacity_then_refills() {
        let mut bucket = TokenBucket::new(RateSpec {
            limit: 3,
            window: Duration::from_secs(1),
        });
        let t0 = Instant::now();
        assert!(bucket.allow_at(t0));
        assert!(bucket.allow_at(t0));
        assert!(bucket.allow_at(t0));
        assert!(!bucket.allow_at(t0));
        let retry = bucket.retry_after_at(t0);
        assert!(retry > Duration::ZERO);
        assert!(bucket.allow_at(t0 + Duration::from_millis(400)));
    }

    #[test]
    fn limiter_is_shared() {
        let limiter = RateLimiter::new(RateSpec {
            limit: 2,
            window: Duration::from_secs(60),
        });
        assert!(limiter.check().is_ok());
        assert!(limiter.check().is_ok());
        assert!(limiter.check().is_err());
    }

    #[test]
    fn display_roundtrip() {
        let spec = "120/min".parse::<RateSpec>().unwrap();
        assert_eq!(spec.to_string(), "120/min");
    }
}
