//! Worker-thread helpers for the `parallel` feature.

/// Worker count for `Encoder::threads`: the requested count, or with 0 the
/// available cores up to 8. Always 1 without the `parallel` feature.
#[cfg(feature = "parallel")]
pub(crate) fn threads(requested: usize) -> usize {
    if requested != 0 {
        return requested;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
}

#[cfg(not(feature = "parallel"))]
pub(crate) fn threads(_requested: usize) -> usize {
    1
}

/// Maps `job` over `items` on up to `threads` workers (the calling thread
/// included) and returns the results in item order. Workers take items in
/// order, so put the most expensive ones first. A panic in `job` is
/// propagated.
#[cfg_attr(not(feature = "parallel"), allow(unused_variables))]
pub(crate) fn map<T: Sync, R: Send>(
    threads: usize,
    items: &[T],
    job: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    #[cfg(feature = "parallel")]
    if threads.min(items.len()) > 1 {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Each worker claims the next index from a shared counter; results
        // are tagged with it and sorted back into item order at the end.
        let next = AtomicUsize::new(0);
        let work = || {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                done.push((i, job(item)));
            }
            done
        };
        let mut tagged: Vec<(usize, R)> = std::thread::scope(|sc| {
            let handles: Vec<_> = (1..threads.min(items.len()))
                .map(|_| sc.spawn(work))
                .collect();
            let mut all = work();
            for h in handles {
                match h.join() {
                    Ok(done) => all.extend(done),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
            all
        });
        tagged.sort_unstable_by_key(|t| t.0);
        return tagged.into_iter().map(|t| t.1).collect();
    }
    items.iter().map(job).collect()
}

/// Runs `job` on every item, one scoped thread per item (the first one on the
/// calling thread). Without the `parallel` feature it is a plain loop.
pub(crate) fn for_each<T: Send, F: Fn(T) + Sync>(items: impl Iterator<Item = T>, job: F) {
    #[cfg(feature = "parallel")]
    {
        let mut items = items;
        let Some(first) = items.next() else { return };
        let job = &job;
        std::thread::scope(|sc| {
            for item in items {
                sc.spawn(move || job(item));
            }
            job(first);
        });
    }
    #[cfg(not(feature = "parallel"))]
    items.for_each(job);
}
