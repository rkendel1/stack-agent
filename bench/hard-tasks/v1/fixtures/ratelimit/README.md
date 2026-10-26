# ratelimit

Rate limiting primitives:

* `TokenBucket`: a bucket of `capacity` tokens refilled at `refill_per_sec`; a request takes
  tokens or is refused.
* `SlidingWindow`: at most `limit` requests in any window of `window_ms` milliseconds.
* `Policy`: parsed from text such as `100/1m`, `5/10s` or `20/500ms burst=40`.
* `KeyedLimiter`: one bucket per key (an API key, a client address).

Time comes from a `Clock`, so everything is testable without sleeping.

Units: `ms`, `s`, `m` (minutes), `h`. A bare number of a unit means one: `100/m` is `100/1m`.
