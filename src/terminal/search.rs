//! 终端滚动历史搜索、结果导航与可见高亮快照。

use alacritty_terminal::{
    event::EventListener,
    grid::{Dimensions, Scroll},
    index::{Column, Direction, Line, Point},
    term::{
        Term,
        search::{Match, RegexIter, RegexSearch},
    },
};
use std::time::{Duration, Instant};

/// 搜索框打开期间，最多每隔该时间重新扫描一次持续变化的终端内容。
const LIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchHighlight {
    Match,
    Current,
}

/// 随帧发送的搜索元数据与当前视口内的匹配范围。
#[derive(Clone, Debug, Default)]
pub(crate) struct SearchSnapshot {
    pub(crate) query: String,
    pub(crate) current: usize,
    pub(crate) total: usize,
    visible_matches: Vec<(Match, SearchHighlight)>,
}

impl SearchSnapshot {
    pub(crate) fn highlight_at(&self, point: Point) -> Option<SearchHighlight> {
        self.visible_matches
            .iter()
            .find_map(|(range, highlight)| range.contains(&point).then_some(*highlight))
    }
}

/// 每个终端会话拥有独立的搜索结果和导航位置。
pub(super) struct SearchState {
    query: String,
    matches: Vec<Match>,
    current: Option<usize>,
    last_refresh: Instant,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            last_refresh: Instant::now(),
        }
    }
}

impl SearchState {
    pub(super) fn query(&self) -> &str {
        &self.query
    }

    /// 更新纯文本查询。匹配默认忽略大小写，与 VS Code 终端查找的默认行为一致。
    pub(super) fn set_query<T: EventListener>(&mut self, terminal: &mut Term<T>, query: String) {
        self.query = query;
        self.rebuild(terminal, None, true);
    }

    /// 在全部匹配项间循环导航，previous 为 true 时向上查找。
    pub(super) fn step<T: EventListener>(&mut self, terminal: &mut Term<T>, previous: bool) {
        if self.matches.is_empty() {
            self.current = None;
            return;
        }
        self.current = Some(match (self.current, previous) {
            (Some(0), true) | (None, true) => self.matches.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % self.matches.len(),
            (None, false) => 0,
        });
        self.scroll_current_into_view(terminal);
    }

    /// 终端仍在输出时节流刷新结果，避免对大段滚动历史进行高频全量扫描。
    pub(super) fn refresh_if_due<T: EventListener>(&mut self, terminal: &mut Term<T>) -> bool {
        if self.query.is_empty() || self.last_refresh.elapsed() < LIVE_REFRESH_INTERVAL {
            return false;
        }
        let previous_matches = self.matches.clone();
        let previous_current = self.current;
        self.rebuild(terminal, previous_current, false);
        previous_matches != self.matches || previous_current != self.current
    }

    pub(super) fn snapshot<T: EventListener>(&self, terminal: &Term<T>) -> SearchSnapshot {
        let display_offset = terminal.grid().display_offset().min(i32::MAX as usize) as i32;
        let visible_top = Line(-display_offset);
        let visible_bottom = visible_top + terminal.screen_lines().saturating_sub(1);
        let visible_matches = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, range)| {
                range.end().line >= visible_top && range.start().line <= visible_bottom
            })
            .map(|(index, range)| {
                let highlight = if self.current == Some(index) {
                    SearchHighlight::Current
                } else {
                    SearchHighlight::Match
                };
                (range.clone(), highlight)
            })
            .collect();
        SearchSnapshot {
            query: self.query.clone(),
            current: self.current.map_or(0, |index| index + 1),
            total: self.matches.len(),
            visible_matches,
        }
    }

    fn rebuild<T: EventListener>(
        &mut self,
        terminal: &mut Term<T>,
        preferred_index: Option<usize>,
        reveal_match: bool,
    ) {
        self.last_refresh = Instant::now();
        self.matches.clear();
        self.current = None;
        if self.query.is_empty() {
            return;
        }

        let pattern = format!("(?i:{})", escape_regex_literal(&self.query));
        let Ok(mut regex) = RegexSearch::new(&pattern) else {
            return;
        };
        let start = Point::new(terminal.topmost_line(), Column(0));
        let end = Point::new(terminal.bottommost_line(), terminal.last_column());
        self.matches = RegexIter::new(start, end, Direction::Right, terminal, &mut regex).collect();
        if self.matches.is_empty() {
            return;
        }

        self.current = preferred_index
            .map(|index| index.min(self.matches.len() - 1))
            .or_else(|| self.nearest_visible_match(terminal))
            .or(Some(0));
        if reveal_match {
            self.scroll_current_into_view(terminal);
        }
    }

    fn nearest_visible_match<T: EventListener>(&self, terminal: &Term<T>) -> Option<usize> {
        let display_offset = terminal.grid().display_offset().min(i32::MAX as usize) as i32;
        let visible_top = Line(-display_offset);
        let visible_bottom = visible_top + terminal.screen_lines().saturating_sub(1);
        self.matches
            .iter()
            .rposition(|range| {
                range.end().line >= visible_top && range.start().line <= visible_bottom
            })
            .or_else(|| {
                self.matches
                    .iter()
                    .rposition(|range| range.end().line < visible_top)
            })
            .or_else(|| {
                self.matches
                    .iter()
                    .position(|range| range.start().line > visible_bottom)
            })
    }

    fn scroll_current_into_view<T: EventListener>(&self, terminal: &mut Term<T>) {
        let Some(range) = self.current.and_then(|index| self.matches.get(index)) else {
            return;
        };
        let current_offset = terminal.grid().display_offset().min(i32::MAX as usize) as i32;
        let visible_top = Line(-current_offset);
        let visible_bottom = visible_top + terminal.screen_lines().saturating_sub(1);
        if range.end().line >= visible_top && range.start().line <= visible_bottom {
            return;
        }

        let center = terminal.screen_lines().saturating_sub(1) / 2;
        let target = (center as i64 - i64::from(range.start().line.0)).max(0) as usize;
        let history_lines = terminal
            .total_lines()
            .saturating_sub(terminal.screen_lines());
        let target = target.min(history_lines);
        let delta = target as i64 - i64::from(current_offset);
        terminal.scroll_display(Scroll::Delta(
            delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32
        ));
    }
}

fn escape_regex_literal(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for character in query.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_regex_literal;

    #[test]
    fn plain_text_search_escapes_regex_syntax() {
        assert_eq!(
            escape_regex_literal(r"a.*[b] (c)? \\"),
            r"a\.\*\[b\] \(c\)\? \\\\"
        );
    }
}
