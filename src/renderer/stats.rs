use crate::terminal::FullRedrawReason;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct PerfStats {
    interval_started: Option<Instant>,
    applied_frames: u64,
    rendered_frames: u64,
    patch_full_redraws: u64,
    full_redraw_reasons: [u64; 3],
    full_surface_renders: u64,
    input_dirty_rows: u64,
    rendered_dirty_rows: u64,
    processed_cells: u64,
    text_spans: u64,
    uploaded_bytes: u64,
    apply_time: Duration,
    render_cpu_time: Duration,
    apply_samples_us: Vec<u64>,
    render_samples_us: Vec<u64>,
}

impl PerfStats {
    pub(super) fn record_apply(
        &mut self,
        full_redraw_reason: Option<FullRedrawReason>,
        dirty_rows: usize,
        cells: usize,
        text_spans: usize,
        bytes: usize,
        elapsed: Duration,
    ) {
        self.start_interval();
        self.applied_frames += 1;
        if let Some(reason) = full_redraw_reason {
            self.patch_full_redraws += 1;
            self.full_redraw_reasons[reason_index(reason)] += 1;
        }
        self.input_dirty_rows += dirty_rows as u64;
        self.processed_cells += cells as u64;
        self.text_spans += text_spans as u64;
        self.uploaded_bytes += bytes as u64;
        self.apply_time += elapsed;
        self.apply_samples_us.push(elapsed.as_micros() as u64);
    }

    pub(super) fn record_render(
        &mut self,
        full_surface: bool,
        dirty_rows: usize,
        elapsed: Duration,
    ) {
        self.start_interval();
        self.rendered_frames += 1;
        self.full_surface_renders += u64::from(full_surface);
        self.rendered_dirty_rows += dirty_rows as u64;
        self.render_cpu_time += elapsed;
        self.render_samples_us.push(elapsed.as_micros() as u64);
        self.log_if_due();
    }

    fn start_interval(&mut self) {
        self.interval_started.get_or_insert_with(Instant::now);
    }

    fn log_if_due(&mut self) {
        let Some(started) = self.interval_started else {
            return;
        };
        if started.elapsed() < Duration::from_secs(1) {
            return;
        }

        eprintln!(
            "terminal-perf applied_frames={} rendered_frames={} patch_full_redraws={} full_surface_renders={} full_reasons=terminal_damage:{},renderer_request:{},resize:{} input_dirty_rows={} rendered_dirty_rows={} processed_cells={} text_spans={} upload_kib={:.1} apply_cpu_us_avg={} apply_cpu_us_p95={} apply_cpu_us_p99={} render_cpu_us_avg={} render_cpu_us_p95={} render_cpu_us_p99={}",
            self.applied_frames,
            self.rendered_frames,
            self.patch_full_redraws,
            self.full_surface_renders,
            self.full_redraw_reasons[0],
            self.full_redraw_reasons[1],
            self.full_redraw_reasons[2],
            self.input_dirty_rows,
            self.rendered_dirty_rows,
            self.processed_cells,
            self.text_spans,
            self.uploaded_bytes as f64 / 1024.0,
            average_us(self.apply_time, self.applied_frames),
            percentile(&self.apply_samples_us, 95),
            percentile(&self.apply_samples_us, 99),
            average_us(self.render_cpu_time, self.rendered_frames),
            percentile(&self.render_samples_us, 95),
            percentile(&self.render_samples_us, 99),
        );
        *self = Self {
            interval_started: Some(Instant::now()),
            ..Self::default()
        };
    }
}

fn reason_index(reason: FullRedrawReason) -> usize {
    match reason {
        FullRedrawReason::TerminalDamage => 0,
        FullRedrawReason::RendererRequest => 1,
        FullRedrawReason::Resize => 2,
    }
}

fn average_us(duration: Duration, count: u64) -> u128 {
    duration.as_micros() / u128::from(count.max(1))
}

fn percentile(samples: &[u64], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_nearest_rank() {
        let samples = (1..=100).collect::<Vec<_>>();
        assert_eq!(percentile(&samples, 95), 95);
        assert_eq!(percentile(&samples, 99), 99);
        assert_eq!(percentile(&[], 95), 0);
    }
}
