use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct PerfStats {
    interval_started: Option<Instant>,
    frames: u64,
    full_redraws: u64,
    dirty_rows: u64,
    cell_allocs: u64,
    uploaded_bytes: u64,
    apply_time: Duration,
    render_time: Duration,
}

impl PerfStats {
    pub(super) fn record_apply(
        &mut self,
        full_redraw: bool,
        dirty_rows: usize,
        cells: usize,
        bytes: usize,
        elapsed: Duration,
    ) {
        self.interval_started.get_or_insert_with(Instant::now);
        self.frames += 1;
        self.full_redraws += u64::from(full_redraw);
        self.dirty_rows += dirty_rows as u64;
        self.cell_allocs += cells as u64;
        self.uploaded_bytes += bytes as u64;
        self.apply_time += elapsed;
    }

    pub(super) fn record_render(&mut self, elapsed: Duration) {
        self.render_time += elapsed;
        let Some(started) = self.interval_started else {
            return;
        };
        if started.elapsed() < Duration::from_secs(1) {
            return;
        }

        let frames = self.frames.max(1);
        eprintln!(
            "terminal-perf frames={} full_redraws={} dirty_rows={} cell_allocs={} upload_kib={:.1} apply_us_avg={} render_submit_us_avg={}",
            self.frames,
            self.full_redraws,
            self.dirty_rows,
            self.cell_allocs,
            self.uploaded_bytes as f64 / 1024.0,
            self.apply_time.as_micros() / u128::from(frames),
            self.render_time.as_micros() / u128::from(frames),
        );
        *self = Self {
            interval_started: Some(Instant::now()),
            ..Self::default()
        };
    }
}
