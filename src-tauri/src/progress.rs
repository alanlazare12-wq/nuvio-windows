use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct SpeedEstimator {
    samples: VecDeque<(Instant, i64)>,
    window: Duration,
    min_sample: Duration,
}

impl SpeedEstimator {
    pub fn new(initial_bytes: i64) -> Self {
        let mut samples = VecDeque::new();
        samples.push_back((Instant::now(), initial_bytes.max(0)));
        Self {
            samples,
            window: Duration::from_secs(5),
            min_sample: Duration::from_millis(850),
        }
    }

    pub fn update(&mut self, bytes: i64) -> i64 {
        let now = Instant::now();
        let bytes = bytes.max(0);
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2 {
            let Some((time, _)) = self.samples.get(1) else {
                break;
            };
            if now.duration_since(*time) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let Some((start_time, start_bytes)) = self.samples.front().copied() else {
            return 0;
        };
        let elapsed = now.duration_since(start_time);
        if elapsed < self.min_sample {
            return 0;
        }
        let delta = bytes.saturating_sub(start_bytes);
        if delta <= 0 {
            return 0;
        }
        (delta as f64 / elapsed.as_secs_f64()) as i64
    }

    pub fn eta(total: i64, processed: i64, speed_bps: i64) -> Option<i64> {
        if processed >= total && total > 0 {
            return Some(0);
        }
        if total <= 0 || processed <= 0 || speed_bps <= 0 {
            return None;
        }
        let remaining = total.saturating_sub(processed).max(0);
        Some((remaining + speed_bps - 1) / speed_bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_requires_real_speed() {
        assert_eq!(SpeedEstimator::eta(100, 0, 0), None);
        assert_eq!(SpeedEstimator::eta(100, 50, 10), Some(5));
        assert_eq!(SpeedEstimator::eta(100, 100, 10), Some(0));
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    #[default]
    Idle,
    Starting,
    Scanning,
    Folders,
    Files,
    Applying,
    Complete,
    Error,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub active: bool,
    pub phase: SyncPhase,
    pub scanned: usize,
    pub total: Option<usize>,
    pub percent: Option<u8>,
    pub eta_seconds: Option<u64>,
    pub error: Option<String>,
}

pub struct SyncRun<'a> {
    state: &'a std::sync::Mutex<SyncProgress>,
    started: Instant,
    finished: bool,
}
impl<'a> SyncRun<'a> {
    pub fn new(state: &'a std::sync::Mutex<SyncProgress>) -> Self {
        *state.lock().expect("sync progress") = SyncProgress {
            active: true,
            phase: SyncPhase::Scanning,
            ..Default::default()
        };
        Self {
            state,
            started: Instant::now(),
            finished: false,
        }
    }
    pub fn phase(&self, phase: SyncPhase) {
        let mut state = self.state.lock().expect("sync progress");
        state.phase = phase;
        state.eta_seconds = None;
        if phase == SyncPhase::Folders {
            state.percent = Some(3);
        }
    }
    pub fn scanned(&self, scanned: usize, total: Option<usize>) {
        let mut state = self.state.lock().expect("sync progress");
        state.phase = SyncPhase::Files;
        state.scanned = scanned;
        state.total = total;
        state.percent = total
            .filter(|n| *n > 0)
            .map(|n| ((scanned as f64 / n as f64 * 90.0) as u8).min(89));
        state.eta_seconds = total.filter(|n| *n > scanned && scanned > 0).map(|n| {
            (self.started.elapsed().as_secs_f64() * (n - scanned) as f64 / scanned as f64).ceil()
                as u64
        });
    }
    pub fn applying(&self, processed: usize, total: usize) {
        let mut state = self.state.lock().expect("sync progress");
        state.phase = SyncPhase::Applying;
        state.percent = Some(90 + (processed.saturating_mul(9) / total.max(1)).min(9) as u8);
        state.eta_seconds = None;
    }
    pub fn finish(&mut self, error: Option<String>) {
        let mut state = self.state.lock().expect("sync progress");
        state.active = false;
        state.phase = if error.is_some() {
            SyncPhase::Error
        } else {
            SyncPhase::Complete
        };
        if error.is_none() {
            state.percent = Some(100);
            state.eta_seconds = Some(0);
        }
        state.error = error;
        self.finished = true;
    }
}
impl Drop for SyncRun<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Some("Sincronización interrumpida".into()));
        }
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    #[test]
    fn folders_phase_precedes_file_progress() {
        let state = std::sync::Mutex::new(SyncProgress::default());
        let run = SyncRun::new(&state);
        run.phase(SyncPhase::Folders);
        {
            let snapshot = state.lock().unwrap();
            assert_eq!(snapshot.phase, SyncPhase::Folders);
            assert_eq!(snapshot.percent, Some(3));
            assert_eq!(snapshot.scanned, 0);
        }
        run.scanned(25, Some(100));
        let snapshot = state.lock().unwrap();
        assert_eq!(snapshot.phase, SyncPhase::Files);
        assert_eq!(snapshot.scanned, 25);
    }

    #[test]
    fn progress_completes_only_after_success() {
        let state = std::sync::Mutex::new(SyncProgress::default());
        let mut run = SyncRun::new(&state);
        run.scanned(200, Some(100));
        assert_eq!(state.lock().unwrap().percent, Some(89));
        run.applying(4, 4);
        assert_eq!(state.lock().unwrap().percent, Some(99));
        run.finish(None);
        assert_eq!(state.lock().unwrap().percent, Some(100));
        assert!(!state.lock().unwrap().active);
    }
    #[test]
    fn unknown_total_and_interruption_do_not_fake_completion() {
        let state = std::sync::Mutex::new(SyncProgress::default());
        {
            let run = SyncRun::new(&state);
            run.scanned(10, None);
            assert_eq!(state.lock().unwrap().percent, None);
        }
        let state = state.lock().unwrap();
        assert!(!state.active);
        assert_eq!(state.phase, SyncPhase::Error);
        assert!(state.error.is_some());
    }
}
