//! Reading the candidate files a search walked: one backend call per page where the substrate can
//! do it, and a BOUNDED fallback when that call fails.
//!
//! The fallback is where a turn went to die: a page whose batch call failed was retried one file
//! at a time, and on a channel that answers every call in ten seconds, 1 500 files is four hours
//! with no result and nothing to kill (R194). So a run of failed reads ends the fallback, the
//! call's wall clock ends it too, and what came back says which of the two stopped it.

use crate::backend::BackendError;
use crate::runtime::InvokeCtx;

/// How many single-file reads may fail before the scan gives up on the channel. Small on purpose:
/// a substrate that cannot answer a page and then fails four reads in a row is broken, and asking
/// it another 1 500 times is the defect this bounds.
pub const FALLBACK_FAILED_READS: usize = 4;

/// Why a scan stopped before its tree ran out. Both are reported as a result of their own: a
/// search that stopped early must never come back reading like a search that finished.
#[derive(Debug, Clone)]
pub enum Stop {
    /// The call spent its wall clock: [`crate::runtime::Limits::call_timeout`].
    Time,
    /// The substrate stopped answering. The string is the last error it gave.
    Channel(String),
}

impl Stop {
    /// The sentence the result carries, in one line, naming the cause and the numbers.
    pub fn sentence(&self, opened: usize, wanted: usize) -> String {
        match self {
            Stop::Time => format!(
                "this search gave up when its wall clock ran out: {opened} of {wanted} file(s) \
                 were opened and the rest were never read"
            ),
            Stop::Channel(why) => format!(
                "the substrate stopped answering, so {opened} of {wanted} file(s) were opened and \
                 the rest were never read (last error: {why})"
            ),
        }
    }
}

/// One scan's reader, holding the facts a page needs at the call level rather than per page: how
/// many reads have failed, and whether the scan has already stopped.
#[derive(Default)]
pub struct Reader {
    failed: usize,
    stop: Option<Stop>,
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Why the scan stopped, once it has. A caller that sees this must report it: what it holds is
    /// part of the answer, never the whole of it.
    pub fn stop(&self) -> Option<&Stop> {
        self.stop.as_ref()
    }

    /// One page of paths, answered in the same order. A page the backend cannot batch is read one
    /// file at a time - and that retry is what the two bounds above exist for.
    pub fn page(
        &mut self,
        ctx: &InvokeCtx<'_>,
        paths: &[&str],
    ) -> Vec<Result<Vec<u8>, BackendError>> {
        if self.stop.is_none() && ctx.out_of_time() {
            self.stop = Some(Stop::Time);
        }
        if let Some(stop) = &self.stop {
            return paths.iter().map(|p| Err(not_read(stop, p))).collect();
        }
        match ctx.backend.read_many(paths) {
            Ok(v) if v.len() == paths.len() => v,
            _ => {
                let mut out = Vec::with_capacity(paths.len());
                for p in paths {
                    if self.stop.is_none() && ctx.out_of_time() {
                        self.stop = Some(Stop::Time);
                    }
                    if let Some(stop) = &self.stop {
                        out.push(Err(not_read(stop, p)));
                        continue;
                    }
                    match ctx.backend.read(p) {
                        Ok(bytes) => out.push(Ok(bytes)),
                        // A file that is gone is a fact about the file, not about the channel:
                        // the search skips it and counts it as unopened, as it always has.
                        Err(e @ BackendError::NotFound(_)) => out.push(Err(e)),
                        Err(BackendError::Io(why)) => {
                            self.failed += 1;
                            if self.failed >= FALLBACK_FAILED_READS {
                                self.stop = Some(Stop::Channel(why.clone()));
                            }
                            out.push(Err(BackendError::Io(why)));
                        }
                        Err(e) => out.push(Err(e)),
                    }
                }
                out
            }
        }
    }
}

/// A path the scan never asked for, carrying the reason so a caller that renders its errors says
/// the same thing the note does.
fn not_read(stop: &Stop, path: &str) -> BackendError {
    BackendError::Io(match stop {
        Stop::Time => format!("`{path}` was not read: the search ran out of wall clock"),
        Stop::Channel(why) => {
            format!("`{path}` was not read: the substrate stopped answering: {why}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Stop;

    /// Both sentences carry the denominator, because "some files were read" is not an answer and
    /// the model's next move depends on how much of the tree this call actually saw.
    #[test]
    fn a_stop_names_what_it_did_and_did_not_open() {
        let time = Stop::Time.sentence(64, 1500);
        assert!(time.contains("64 of 1500"), "{time}");
        let channel = Stop::Channel("the VM went away".into()).sentence(0, 1500);
        assert!(channel.contains("0 of 1500"), "{channel}");
        assert!(channel.contains("the VM went away"), "{channel}");
    }
}
