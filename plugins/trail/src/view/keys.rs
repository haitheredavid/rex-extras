//! Keys and clicks. Letters mean the same in every orientation (`j`/`k` agents, `h`/`l` time);
//! arrows are spatial, so they follow the agents' axis: rows in ↔, columns in ↕.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use super::prompt::{Prompt, PromptKind};
use super::timeline::{self, Surface};
use super::{App, AreaMode, Axis, BUCKETS_S, Density, Hit, Lens, Mode, Orient, blocked_lanes, sequence};
use crate::model::{Group, Row, RowKey, Scope, Sort};

/// Two clicks on one target this close together are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        if self.help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => self.help = false,
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            }
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        if matches!(self.surface, Surface::Timeline(_)) {
            timeline::on_key(self, key);
            return;
        }
        let rows = self.orient == Orient::Horizontal;
        let code = match (key.code, rows) {
            (KeyCode::Down, true) | (KeyCode::Right, false) => KeyCode::Char('j'),
            (KeyCode::Up, true) | (KeyCode::Left, false) => KeyCode::Char('k'),
            (KeyCode::Left, true) | (KeyCode::Up, false) => KeyCode::Char('h'),
            (KeyCode::Right, true) | (KeyCode::Down, false) => KeyCode::Char('l'),
            (c, _) => c,
        };
        match code {
            KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(self.shown.1.max(1) as isize),
            KeyCode::PageUp => self.move_selection(-(self.shown.1.max(1) as isize)),
            KeyCode::Home => self.move_selection(isize::MIN / 2),
            KeyCode::End => self.move_selection(isize::MAX / 2),
            KeyCode::Char('J') => self.next_group(true),
            KeyCode::Char('K') => self.next_group(false),
            KeyCode::Char('b') => self.next_blocked(),
            KeyCode::Char('i') => self.inspector = Some(!self.inspector_shown),
            KeyCode::Char('I') => timeline::open(self),
            KeyCode::Char('h') => self.scroll_time(true, false),
            KeyCode::Char('l') => self.scroll_time(false, false),
            KeyCode::Char('H') => self.scroll_time(true, true),
            KeyCode::Char('L') => self.scroll_time(false, true),
            KeyCode::Tab | KeyCode::BackTab => {
                self.mode = match self.mode {
                    Mode::Lanes => Mode::Usage,
                    Mode::Usage => Mode::Lanes,
                }
            }
            KeyCode::Char(c @ '1'..='5') => self.preset(c as u8 - b'0'),
            KeyCode::Char('v') => match self.mode {
                Mode::Lanes => {
                    self.lens = match self.lens {
                        Lens::Status => Lens::Heat,
                        Lens::Heat => Lens::Ctx,
                        Lens::Ctx => Lens::Status,
                    }
                }
                Mode::Usage => {
                    self.area_mode = match self.area_mode {
                        AreaMode::Stacked => AreaMode::Overlaid,
                        AreaMode::Overlaid => AreaMode::Stacked,
                    }
                }
            },
            KeyCode::Char('t') if self.mode == Mode::Lanes && self.lens == Lens::Status => {
                self.axis = match self.axis {
                    Axis::Time => Axis::Steps,
                    Axis::Steps => Axis::Time,
                }
            }
            KeyCode::Char(' ') => match self.selected_row() {
                Some(Row::Group { key, .. }) => {
                    self.query.collapsed.remove(&key);
                }
                Some(Row::Lane(li)) => {
                    let id = self.state.lanes[li].id;
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                }
                _ => {}
            },
            KeyCode::Char('Z') => {
                self.density = match self.density {
                    Density::Auto => Density::Compact,
                    Density::Compact => Density::Roomy,
                    Density::Roomy => Density::Auto,
                }
            }
            KeyCode::Char('o') => {
                self.orient = match self.orient {
                    Orient::Horizontal => Orient::Vertical,
                    Orient::Vertical => Orient::Horizontal,
                };
                self.lane_scroll = 0;
            }
            KeyCode::Char('/') => self.prompt = Some(Prompt { kind: PromptKind::Search, text: self.query.search.clone() }),
            KeyCode::Char('a') => self.query.active_only = !self.query.active_only,
            KeyCode::Char('w') => {
                self.query.scope = match self.query.scope {
                    Scope::All => Scope::Workspace,
                    Scope::Workspace => Scope::Tab,
                    Scope::Tab => Scope::All,
                }
            }
            KeyCode::Char('s') => {
                self.query.sort = match self.query.sort {
                    Sort::Stable => Sort::Attention,
                    Sort::Attention => Sort::Recent,
                    Sort::Recent => Sort::Name,
                    Sort::Name => Sort::Stable,
                }
            }
            KeyCode::Char('g') => {
                self.query.group = match self.query.group {
                    Group::Workspace => Group::Tab,
                    Group::Tab => Group::None,
                    Group::None => Group::Workspace,
                };
                self.lane_scroll = 0;
            }
            KeyCode::Char('x') => self.fold_group(),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                // One layer at a time: the search, then an expanded lane, then the popup itself.
                let expanded = self.selected_lane().map(|li| self.state.lanes[li].id).filter(|id| self.expanded.contains(id));
                if !self.query.search.is_empty() {
                    self.query.search.clear();
                } else if let Some(id) = expanded {
                    self.expanded.remove(&id);
                } else if self.popup {
                    self.quit = true;
                }
            }
            KeyCode::Char('r') => self.refresh_requested = true,
            KeyCode::Char('c') => self.query.show_closed = !self.query.show_closed,
            KeyCode::Enter => match self.selected_row() {
                Some(Row::Group { key, .. }) => {
                    self.query.collapsed.remove(&key);
                }
                Some(Row::Lane(li)) => self.focus_requested = Some(self.state.lanes[li].pane_id.clone()),
                _ => {}
            },
            KeyCode::Char('f') => {
                self.follow = true;
                self.seq_back = 0;
            }
            KeyCode::Char('0') => {
                self.bucket_auto = true;
                self.follow = true;
                self.seq_back = 0;
            }
            KeyCode::Char('+') | KeyCode::Char('=') if !self.steps() => {
                self.bucket_auto = false;
                self.bucket_idx = self.bucket_idx.saturating_sub(1);
            }
            KeyCode::Char('-') if !self.steps() => {
                self.bucket_auto = false;
                self.bucket_idx = (self.bucket_idx + 1).min(BUCKETS_S.len() - 1);
            }
            _ => {}
        }
    }

    /// Typing into the prompt; a search filters as it changes.
    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        let search = p.kind == PromptKind::Search;
        match key.code {
            KeyCode::Char(c) => p.text.push(c),
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Esc => {
                self.prompt = None;
                if search {
                    self.query.search.clear();
                }
                return;
            }
            KeyCode::Enter => {
                let p = self.prompt.take().expect("checked above");
                match p.kind {
                    PromptKind::Search => self.query.search = p.text,
                    PromptKind::JumpTool => timeline::accept_tool(self, p.text),
                }
                return;
            }
            _ => return,
        }
        if search {
            self.query.search = p.text.clone();
        }
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        self.sel_row(&rows).map(|r| rows[r].clone())
    }

    /// Moves the selection `by` rows, clamped to the first and last (the summary isn't a stop).
    fn move_selection(&mut self, by: isize) {
        let rows = self.rows();
        let last = rows.iter().filter(|r| !matches!(r, Row::Summary { .. })).count();
        if let Some(r) = self.sel_row(&rows) {
            let r = (r as isize).saturating_add(by).clamp(0, last as isize - 1) as usize;
            self.select_row(&rows, r);
        }
    }

    /// Group key of each row (None for the summary).
    fn group_keys(&self, rows: &[Row]) -> Vec<Option<String>> {
        rows.iter()
            .map(|r| match r {
                Row::Group { key, .. } => Some(key.clone()),
                Row::Lane(li) => Some(self.state.group_of(*li, self.query.group).0),
                Row::Summary { .. } => None,
            })
            .collect()
    }

    /// `J`/`K`: the first row of the next or previous group (its first lane, under an open header).
    fn next_group(&mut self, forward: bool) {
        let rows = self.rows();
        let keys = self.group_keys(&rows);
        let Some(cur) = self.sel_row(&rows) else { return };
        let start_of = |k: usize| (0..=k).rev().take_while(|&i| keys[i] == keys[k]).last().unwrap_or(k);
        let target = if forward {
            (cur + 1..rows.len()).find(|&i| keys[i].is_some() && keys[i] != keys[cur])
        } else {
            start_of(cur).checked_sub(1).map(start_of)
        };
        let Some(mut t) = target else { return };
        if matches!(rows[t], Row::Group { collapsed: false, .. }) && matches!(rows.get(t + 1), Some(Row::Lane(_))) {
            t += 1;
        }
        self.select_row(&rows, t);
    }

    /// `x`: folds or unfolds the selected row's group, keeping the selection on its header.
    fn fold_group(&mut self) {
        let rows = self.rows();
        let Some(cur) = self.sel_row(&rows) else { return };
        let Some(key) = self.group_keys(&rows)[cur].clone() else { return };
        if self.query.group == Group::None {
            return;
        }
        if !self.query.collapsed.remove(&key) {
            self.query.collapsed.insert(key.clone());
        }
        self.selected = Some(RowKey::Group(key));
    }

    /// `b`: the next blocked lane after the selection, in row order, wrapping.
    fn next_blocked(&mut self) {
        let rows = self.rows();
        let blocked = blocked_lanes(self);
        let hits: Vec<usize> = (0..rows.len()).filter(|&r| matches!(rows[r], Row::Lane(li) if blocked.contains(&li))).collect();
        let cur = self.sel_row(&rows);
        let next = hits.iter().find(|&&r| cur.is_none_or(|c| r > c)).or(hits.first());
        if let Some(&r) = next {
            self.select_row(&rows, r);
        }
    }

    /// Back or forward a quarter of the strip (`full`: a whole strip); on the steps axis, by steps.
    fn scroll_time(&mut self, back: bool, full: bool) {
        if self.steps() {
            let by = if full { (self.time_cells / sequence::step_cells(self.orient)).max(1) } else { 1 };
            if back {
                self.seq_back += by;
            } else {
                self.seq_back = self.seq_back.saturating_sub(by);
            }
            self.follow = self.seq_back == 0;
            return;
        }
        let cells = if full { self.time_cells } else { (self.time_cells / 4).max(1) };
        let step = cells as i64 * self.bucket_ms();
        if back {
            if self.follow {
                self.view_end_ms = self.now_ms;
            }
            self.view_end_ms -= step;
            self.follow = false;
        } else {
            self.view_end_ms += step;
            if self.view_end_ms >= self.now_ms {
                self.follow = true;
            }
        }
    }

    /// Clicks select (a second click on the same target acts as Enter), the mode tabs switch,
    /// the blocked badge jumps to the longest wait; the wheel scrolls rows (with Shift, time).
    pub fn on_mouse(&mut self, m: MouseEvent) {
        let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match m.kind {
            MouseEventKind::ScrollDown => self.on_key(key(if shift { 'l' } else { 'j' })),
            MouseEventKind::ScrollUp => self.on_key(key(if shift { 'h' } else { 'k' })),
            MouseEventKind::Down(MouseButton::Left) => {
                let at = Position::new(m.column, m.row);
                let Some(hit) = self.hits.iter().rev().find(|(r, _)| r.contains(at)).map(|(_, h)| h.clone()) else { return };
                let double = self.last_click.as_ref().is_some_and(|(h, t)| *h == hit && t.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((hit.clone(), Instant::now()));
                match hit {
                    Hit::Row(k) => {
                        let rows = self.rows();
                        if let Some(r) = rows.iter().position(|row| row.key(&self.state).as_ref() == Some(&k)) {
                            self.select_row(&rows, r);
                        }
                        if double {
                            self.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                    }
                    Hit::ModeTab(mode) => self.mode = mode,
                    Hit::Badge => {
                        if let Some(&li) = blocked_lanes(self).first() {
                            self.selected = Some(RowKey::Lane(self.state.lanes[li].id));
                            self.focus_requested = Some(self.state.lanes[li].pane_id.clone());
                        }
                    }
                    Hit::Step(r) => timeline::click(self, r, double),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key, press, render, row, sample_app, sel, two_lanes};
    use super::*;

    #[test]
    fn letters_mean_the_same_in_both_orientations() {
        let mut app = two_lanes(Orient::Vertical);
        key(&mut app, 'j');
        assert_eq!(sel(&app), Some(1), "j is the next agent in columns too");
        key(&mut app, 'h');
        assert!(!app.follow && app.view_end_ms < app.now_ms, "h goes back in time");
        let back = app.view_end_ms;
        key(&mut app, 'k');
        assert_eq!(sel(&app), Some(0));
        press(&mut app, KeyCode::Right);
        assert_eq!(sel(&app), Some(1), "→ is the next column");
        press(&mut app, KeyCode::Up);
        assert!(app.view_end_ms < back, "↑ goes back in time down the columns");

        let mut app = two_lanes(Orient::Horizontal);
        press(&mut app, KeyCode::Down);
        assert_eq!(sel(&app), Some(1), "↓ is the next row");
        press(&mut app, KeyCode::Down);
        assert_eq!(sel(&app), Some(1), "clamped");
        press(&mut app, KeyCode::Home);
        assert_eq!(sel(&app), Some(0));
    }

    #[test]
    fn v_cycles_lenses_and_t_the_axis() {
        let mut app = two_lanes(Orient::Horizontal);
        key(&mut app, 't');
        assert_eq!(app.axis, Axis::Steps);
        key(&mut app, 'v');
        assert_eq!(app.lens, Lens::Heat);
        key(&mut app, 't');
        assert_eq!(app.axis, Axis::Steps, "t only applies to the status lens");
        press(&mut app, KeyCode::Tab);
        key(&mut app, 'v');
        assert_eq!((app.mode, app.area_mode), (Mode::Usage, AreaMode::Overlaid));
    }

    #[test]
    fn esc_unwinds_search_then_expansion_then_the_popup() {
        let mut app = two_lanes(Orient::Horizontal);
        app.popup = true;
        key(&mut app, ' ');
        for c in ['/', 'o', 'm'] {
            key(&mut app, c);
        }
        assert_eq!(app.query.search, "om", "the search filters as it is typed");
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_none() && app.query.search == "om");
        press(&mut app, KeyCode::Esc);
        assert!(app.query.search.is_empty() && app.expanded.len() == 1);
        press(&mut app, KeyCode::Esc);
        assert!(app.expanded.is_empty() && !app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn query_keys_show_in_the_header() {
        let mut app = two_lanes(Orient::Horizontal);
        for c in ['s', 's', 'a'] {
            key(&mut app, c);
        }
        let header = row(&render(&mut app, 160, 6), 0, 0, 160);
        assert!(header.contains("active · sort recent · ungrouped"), "{header}");
    }

    #[test]
    fn x_folds_a_group_and_space_unfolds_it() {
        let mut app = two_lanes(Orient::Horizontal);
        app.query.group = Group::Workspace;
        render(&mut app, 80, 10);
        key(&mut app, 'j');
        key(&mut app, 'x');
        assert!(app.query.collapsed.contains("w1"));
        let buf = render(&mut app, 80, 10);
        assert!(row(&buf, 2, 0, 80).starts_with("▸ w1 · 2 agents"), "{}", row(&buf, 2, 0, 80));
        assert!(row(&buf, 3, 0, 80).contains("blocked"), "the blocked lane stays out");
        assert_eq!(row(&buf, 4, 0, 80), "", "the idle one folds away");
        key(&mut app, ' ');
        assert!(app.query.collapsed.is_empty());
    }

    #[test]
    fn clicking_a_row_selects_it() {
        let mut app = two_lanes(Orient::Horizontal);
        render(&mut app, 80, 6);
        let click = |row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click(3));
        assert_eq!(sel(&app), Some(1));
        assert!(app.focus_requested.is_none());
        app.on_mouse(click(3));
        assert_eq!(app.focus_requested.as_deref(), Some("w1:p2"), "a double-click jumps");
        let tab = app.hits.iter().find(|(_, h)| *h == Hit::ModeTab(Mode::Usage)).map(|(r, _)| *r).unwrap();
        app.on_mouse(MouseEvent { column: tab.x, row: tab.y, ..click(0) });
        assert_eq!(app.mode, Mode::Usage);
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let mut app = sample_app(1, Orient::Horizontal);
        key(&mut app, '?');
        let buf = render(&mut app, 100, 50);
        let body: String = (1..50).map(|y| row(&buf, y, 0, 100)).collect::<Vec<_>>().join("\n");
        assert!(body.contains(" keys ") && body.contains("quit"), "{body}");
        key(&mut app, 'j');
        assert!(app.help, "other keys do nothing while help is open");
        press(&mut app, KeyCode::Esc);
        assert!(!app.help);
        assert!(!app.quit);
    }
}
