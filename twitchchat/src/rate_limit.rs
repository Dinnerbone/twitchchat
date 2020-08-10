#![allow(missing_docs)]
/*!
A simple leaky-bucket style token-based rate limiter
*/
use futures_lite::future::Boxed;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub trait AsyncBlocker: Send + Sync + 'static {
    fn block(&self, duration: Duration) -> Boxed<()>;
}

impl<T: AsyncBlocker + Send + Sync> AsyncBlocker for &'static T {
    fn block(&self, duration: Duration) -> Boxed<()> {
        (*self).block(duration)
    }
}

impl<T: AsyncBlocker + Send> AsyncBlocker for &'static mut T {
    fn block(&self, duration: Duration) -> Boxed<()> {
        (**self).block(duration)
    }
}

impl<T: AsyncBlocker + Send> AsyncBlocker for Box<T> {
    fn block(&self, duration: Duration) -> Boxed<()> {
        (**self).block(duration)
    }
}

impl<T: AsyncBlocker + Send + Sync> AsyncBlocker for Arc<T> {
    fn block(&self, duration: Duration) -> Boxed<()> {
        (**self).block(duration)
    }
}

#[derive(Default, Copy, Clone, Debug)]
pub struct NullBlocker {}

impl AsyncBlocker for NullBlocker {
    fn block(&self, _: Duration) -> Boxed<()> {
        Box::pin(async move {})
    }
}

// struct TokioBlocker {}

// impl Blocker for TokioBlocker {
//     type F = Boxed<'static, ()>;
//     fn block(&self, duration: Duration) -> Self::F
//     where
//         <Self::F as Future>::Output: Send + 'static,
//     {
//         Box::pin(async move {
//             tokio::time::delay_for(duration).await;
//         })
//     }
// }

/// A preset number of tokens as described by Twitch
#[non_exhaustive]
#[derive(Copy, Clone, Debug)]
pub enum RateClass {
    /// `20` per `30` seconds
    Regular,
    /// `100` per `30` seconds
    Moderator,
    /// `50` per `30` seconds
    Known,
    /// `7500` per `30` seconds
    Verified,
}

impl Default for RateClass {
    fn default() -> Self {
        Self::Regular
    }
}

impl RateClass {
    /// Number of tickets available for this class
    pub fn tickets(self) -> u64 {
        match self {
            Self::Regular => 20,
            Self::Moderator => 100,
            Self::Known => 50,
            Self::Verified => 7500,
        }
    }

    /// Period specified by Twitch
    pub const fn period() -> Duration {
        Duration::from_secs(30)
    }
}

/// A leaky-bucket style token-based rate limiter
#[derive(Debug, Clone)]
pub struct RateLimit {
    cap: u64,
    bucket: Bucket,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self::from_class(<_>::default())
    }
}

impl RateLimit {
    /// Create a rate limit from a RateClass
    pub fn from_class(rate_class: RateClass) -> Self {
        Self::full(rate_class.tickets(), RateClass::period())
    }

    /// Create a new rate limiter of `capacity` with an `initial` number of
    /// token and the `period` between refills
    pub fn new(cap: u64, initial: u64, period: Duration) -> Self {
        Self {
            cap,
            bucket: Bucket::new(cap, initial, period),
        }
    }

    /// Create a new rate limiter that is pre-filled
    ///
    /// `cap` is the number of total tokens available
    ///
    /// `period` is how long it'll take to refill all of the tokens
    pub fn full(cap: u64, period: Duration) -> Self {
        Self {
            cap,
            bucket: Bucket::new(cap, cap, period),
        }
    }

    /// Create am empty rate limiter
    ///
    /// `cap` is the number of total tokens available
    ///
    /// `period` is how long it'll take to refill all of the tokens
    ///
    /// This will block, at first, atleast one `period` until its filled
    pub fn empty(cap: u64, period: Duration) -> Self {
        Self {
            cap,
            bucket: Bucket::new(cap, 0, period),
        }
    }

    /// Consume a specific ammount of tokens
    ///
    /// # Returns    
    /// * Successful consumption (e.g. not blocking) will return how many tokens
    ///   are left
    /// * Failure to consume (e.g. out of tokens) will return a Duration of when
    ///   the bucket will be refilled
    pub fn consume(&mut self, tokens: u64) -> Result<u64, Duration> {
        let Self { bucket, .. } = self;

        let now = Instant::now();
        if let Some(n) = bucket.refill(now) {
            bucket.tokens = std::cmp::min(bucket.tokens + n, self.cap);
        }

        if tokens <= bucket.tokens {
            bucket.tokens -= tokens;
            bucket.backoff = 0;
            return Ok(bucket.tokens);
        }

        let prev = bucket.tokens;
        Err(bucket.estimate(tokens - prev, now))
    }

    /// Throttle the current task, consuming a specific amount of tokens and
    /// possibly blocking until tokens are available
    ///
    /// # Returns
    /// the amount of tokens remaining
    pub async fn throttle_async<B>(&mut self, tokens: u64, blocker: &B) -> u64
    where
        B: AsyncBlocker + ?Sized,
    {
        loop {
            match self.consume(tokens) {
                Ok(rem) => return rem,
                Err(time) => {
                    log::debug!("blocking for: {:.3?}", time);
                    blocker.block(time).await;
                }
            }
        }
    }

    /// Throttle the current task, consuming a specific amount of tokens and
    /// possibly blocking until tokens are available
    ///
    /// # Returns
    /// the amount of tokens remaining
    pub fn throttle<F>(&mut self, tokens: u64, blocker: F) -> u64
    where
        F: Fn(Duration),
    {
        loop {
            match self.consume(tokens) {
                Ok(rem) => return rem,
                Err(time) => {
                    log::debug!("blocking for: {:.3?}", time);
                    blocker(time)
                }
            }
        }
    }

    /// Take a single token, returning how many are available
    ///
    /// This'll block the task if none are available
    #[inline]
    pub async fn take_async<B>(&mut self, blocker: &B) -> u64
    where
        B: AsyncBlocker + ?Sized,
    {
        self.throttle_async(1, blocker).await
    }

    /// Take a single token, returning how many are available
    ///
    /// This'll block the task if none are available
    #[inline]
    pub fn take<F>(&mut self, blocker: F) -> u64
    where
        F: Fn(Duration),
    {
        self.throttle(1, blocker)
    }
}

#[derive(Debug, Clone)]
struct Bucket {
    tokens: u64,
    backoff: u32,
    next: Instant,
    last: Instant,
    quantum: u64,
    period: Duration,
}

impl Bucket {
    fn new(tokens: u64, initial: u64, period: Duration) -> Self {
        let now = Instant::now();
        Self {
            tokens: initial,
            backoff: 0,
            next: now + period,
            last: now,
            quantum: tokens,
            period,
        }
    }

    fn refill(&mut self, now: Instant) -> Option<u64> {
        if now < self.next {
            return None;
        }

        let last = now.duration_since(self.last);
        let periods = last.as_nanos().checked_div(self.period.as_nanos())? as u64;
        self.last += self.period * (periods as u32);
        self.next = self.last + self.period;
        (periods * self.quantum).into()
    }

    fn estimate(&mut self, tokens: u64, now: Instant) -> Duration {
        let until = self.next.duration_since(now);
        let periods = (tokens.checked_add(self.quantum).unwrap() - 1) / self.quantum;
        until + self.period * (periods as u32 - 1)
    }
}