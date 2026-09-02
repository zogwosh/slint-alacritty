//! 终端滚动历史搜索、结果导航与视口高亮。
//!
//! 计数与导航使用节流后的全量扫描；高亮只对当前视口执行一次正则，
//! 因此不需要维护随终端输出失效的逐行缓存。

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

/// 搜索框打开期间，最多每隔该时间重新扫描一次持续变化的终端内容以更新计数。
const LIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
/// 全量扫描保留的最大结果数；超过后计数停止增长，避免病态查询占满工作线程。
const MAX_MATCHES: usize = 10_000;
/// 视口高亮向上下各扩展的最大行数，用于捕获跨越自动换行进入视口的匹配。
const MAX_WRAPPED_SEARCH_LINES: i32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchHighlight {
    Match,
    Current,
}

/// 随帧发送给查找面板的搜索元数据；不参与终端渲染缓存。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SearchSnapshot {
    pub(crate) query: String,
    pub(crate) current: usize,
    pub(crate) total: usize,
}

/// 每个终端会话拥有独立的搜索结果和导航位置。
pub(super) struct SearchState {
    query: String,
    regex: Option<RegexSearch>,
    matches: Vec<Match>,
    current: Option<usize>,
    last_refresh: Instant,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            regex: None,
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
        self.regex = if self.query.is_empty() {
            None
        } else {
            let pattern = format!("(?i:{})", escape_regex_literal(&self.query));
            RegexSearch::new(&pattern).ok()
        };
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

    /// 终端仍在输出时节流刷新计数与当前结果位置；高亮由视口搜索独立得出，不依赖此处。
    pub(super) fn refresh_if_due<T: EventListener>(&mut self, terminal: &mut Term<T>) {
        if self.regex.is_none() || self.last_refresh.elapsed() < LIVE_REFRESH_INTERVAL {
            return;
        }
        let previous_current = self.current;
        self.rebuild(terminal, previous_current, false);
    }

    pub(super) fn snapshot(&self) -> SearchSnapshot {
        SearchSnapshot {
            query: self.query.clone(),
            current: self.current.map_or(0, |index| index + 1),
            total: self.matches.len(),
        }
    }

    /// 只在当前视口（连同跨越换行进入视口的部分）执行正则，得到需要高亮的匹配。
    pub(super) fn visible_matches<T: EventListener>(
        &mut self,
        terminal: &Term<T>,
    ) -> Vec<(Match, SearchHighlight)> {
        let Some(regex) = self.regex.as_mut() else {
            return Vec::new();
        };
        let (viewport_top, viewport_bottom) = viewport_lines(terminal);
        let mut start = terminal.line_search_left(Point::new(viewport_top, Column(0)));
        let mut end = terminal.line_search_right(Point::new(viewport_bottom, Column(0)));
        start.line = start.line.max(viewport_top - MAX_WRAPPED_SEARCH_LINES);
        end.line = end.line.min(viewport_bottom + MAX_WRAPPED_SEARCH_LINES);
        let current = self.current.and_then(|index| self.matches.get(index));

        RegexIter::new(start, end, Direction::Right, terminal, regex)
            .skip_while(|range| range.end().line < viewport_top)
            .take_while(|range| range.start().line <= viewport_bottom)
            .map(|range| {
                let highlight = if current == Some(&range) {
                    SearchHighlight::Current
                } else {
                    SearchHighlight::Match
                };
                (range, highlight)
            })
            .collect()
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
        let Some(regex) = self.regex.as_mut() else {
            return;
        };

        let start = Point::new(terminal.topmost_line(), Column(0));
        let end = Point::new(terminal.bottommost_line(), terminal.last_column());
        self.matches = RegexIter::new(start, end, Direction::Right, terminal, regex)
            .take(MAX_MATCHES)
            .collect();
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
        let (visible_top, visible_bottom) = viewport_lines(terminal);
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
        let (visible_top, visible_bottom) = viewport_lines(terminal);
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

/// 当前视口在网格坐标中的首末行。
fn viewport_lines<T: EventListener>(terminal: &Term<T>) -> (Line, Line) {
    let display_offset = terminal.grid().display_offset().min(i32::MAX as usize) as i32;
    let top = Line(-display_offset);
    (top, top + terminal.screen_lines().saturating_sub(1))
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
