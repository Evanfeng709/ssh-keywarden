//! 纯 Rust VT100 / ANSI 终端屏幕仿真器（与手机端 TerminalScreen 同构）。

use eframe::egui::Color32;

const MAX_HISTORY: usize = 2500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtPen {
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for VtPen {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            underline: false,
            inverse: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtCell {
    pub ch: char,
    pub pen: VtPen,
    /// 宽字符（中文/全角）右半格占位
    pub continuation: bool,
}

impl Default for VtCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            pen: VtPen::default(),
            continuation: false,
        }
    }
}

pub fn is_wide_char(c: char) -> bool {
    let u = c as u32;
    (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE10..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x20000..=0x2FFFD).contains(&u)
        || (0x30000..=0x3FFFD).contains(&u)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    Osc,
    OscEsc,
}

#[derive(Clone)]
pub struct VtScreen {
    cols: usize,
    rows: usize,
    main: Vec<Vec<VtCell>>,
    alt: Vec<Vec<VtCell>>,
    history: Vec<Vec<VtCell>>,
    using_alt: bool,
    cx: usize,
    cy: usize,
    saved_main: (usize, usize, VtPen),
    saved_alt: (usize, usize, VtPen),
    top: usize,
    bottom: usize,
    pen: VtPen,
    wrap_pending: bool,
    auto_wrap: bool,
    pub cursor_visible: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub title: String,
    state: ParseState,
    csi_buf: String,
    osc_buf: String,
    utf8_buf: Vec<u8>,
}

impl VtScreen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let c = cols.max(2);
        let r = rows.max(2);
        Self {
            cols: c,
            rows: r,
            main: blank_grid(r, c),
            alt: blank_grid(r, c),
            history: Vec::new(),
            using_alt: false,
            cx: 0,
            cy: 0,
            saved_main: (0, 0, VtPen::default()),
            saved_alt: (0, 0, VtPen::default()),
            top: 0,
            bottom: r - 1,
            pen: VtPen::default(),
            wrap_pending: false,
            auto_wrap: true,
            cursor_visible: true,
            app_cursor: false,
            bracketed_paste: false,
            title: String::new(),
            state: ParseState::Ground,
            csi_buf: String::new(),
            osc_buf: String::new(),
            utf8_buf: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cx, self.cy)
    }

    #[allow(dead_code)]
    pub fn using_alt_screen(&self) -> bool {
        self.using_alt
    }

    pub fn history(&self) -> &[Vec<VtCell>] {
        if self.using_alt {
            &[]
        } else {
            &self.history
        }
    }

    pub fn clear_scrollback(&mut self) {
        self.history.clear();
    }

    pub fn grid(&self) -> &[Vec<VtCell>] {
        if self.using_alt {
            &self.alt
        } else {
            &self.main
        }
    }

    fn grid_mut(&mut self) -> &mut Vec<Vec<VtCell>> {
        if self.using_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    pub fn all_text(&self) -> String {
        let mut lines = Vec::new();
        for row in self.history().iter().chain(self.grid().iter()) {
            let mut s = String::new();
            for cell in row {
                if !cell.continuation {
                    s.push(cell.ch);
                }
            }
            lines.push(s.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let next_cols = cols.max(2);
        let next_rows = rows.max(2);
        if next_cols == self.cols && next_rows == self.rows {
            return;
        }
        self.cols = next_cols;
        self.rows = next_rows;
        self.top = 0;
        self.bottom = self.rows - 1;

        let mut main = std::mem::take(&mut self.main)
            .into_iter()
            .map(|r| reshape_row(r, self.cols))
            .collect::<Vec<_>>();
        while main.len() > self.rows {
            if main.len() - 1 > self.cy && is_row_blank(main.last().unwrap()) {
                main.pop();
                continue;
            }
            let popped = main.remove(0);
            self.push_history(popped);
            if self.cy > 0 {
                self.cy -= 1;
            }
        }
        while main.len() < self.rows {
            main.push(blank_row(self.cols));
        }
        self.main = main;

        let mut alt = std::mem::take(&mut self.alt)
            .into_iter()
            .map(|r| reshape_row(r, self.cols))
            .collect::<Vec<_>>();
        while alt.len() > self.rows {
            alt.pop();
        }
        while alt.len() < self.rows {
            alt.push(blank_row(self.cols));
        }
        self.alt = alt;

        self.cx = self.cx.min(self.cols - 1);
        self.cy = self.cy.min(self.rows - 1);
        self.wrap_pending = false;
    }

    fn push_history(&mut self, row: Vec<VtCell>) {
        self.history.push(row);
        if self.history.len() > MAX_HISTORY {
            let excess = self.history.len() - MAX_HISTORY;
            self.history.drain(0..excess);
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.utf8_buf.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.utf8_buf);
        let mut idx = 0;
        while idx < buf.len() {
            match std::str::from_utf8(&buf[idx..]) {
                Ok(valid) => {
                    for ch in valid.chars() {
                        self.step(ch);
                    }
                    idx = buf.len();
                }
                Err(err) => {
                    let valid_len = err.valid_up_to();
                    if valid_len > 0 {
                        if let Ok(valid) = std::str::from_utf8(&buf[idx..idx + valid_len]) {
                            for ch in valid.chars() {
                                self.step(ch);
                            }
                        }
                        idx += valid_len;
                    }
                    match err.error_len() {
                        Some(bad_len) => {
                            self.step('\u{FFFD}');
                            idx += bad_len;
                        }
                        None => {
                            self.utf8_buf.extend_from_slice(&buf[idx..]);
                            break;
                        }
                    }
                }
            }
        }
    }

    fn step(&mut self, ch: char) {
        match self.state {
            ParseState::Ground => self.ground(ch),
            ParseState::Esc => self.esc(ch),
            ParseState::EscIntermediate => {
                if ('\x30'..='\x7E').contains(&ch) {
                    self.state = ParseState::Ground;
                }
            }
            ParseState::Csi => self.csi(ch),
            ParseState::Osc => {
                if ch == '\x07' {
                    self.finish_osc();
                } else if ch == '\x1B' {
                    self.state = ParseState::OscEsc;
                } else {
                    self.osc_buf.push(ch);
                }
            }
            ParseState::OscEsc => {
                if ch == '\\' {
                    self.finish_osc();
                } else {
                    self.state = ParseState::Ground;
                }
            }
        }
    }

    fn ground(&mut self, ch: char) {
        match ch {
            '\x1B' => self.state = ParseState::Esc,
            '\r' => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            '\n' | '\x0B' | '\x0C' => {
                self.wrap_pending = false;
                self.line_feed();
            }
            '\x08' => {
                self.wrap_pending = false;
                if self.cx > 0 {
                    self.cx -= 1;
                }
            }
            '\t' => {
                self.wrap_pending = false;
                let next = ((self.cx / 8) + 1) * 8;
                self.cx = next.min(self.cols - 1);
            }
            '\x00'..='\x1F' | '\x7F' => {}
            _ => self.put_char(ch),
        }
    }

    fn esc(&mut self, ch: char) {
        match ch {
            '[' => {
                self.csi_buf.clear();
                self.state = ParseState::Csi;
            }
            ']' => {
                self.osc_buf.clear();
                self.state = ParseState::Osc;
            }
            '(' | ')' | '*' | '+' | '#' => {
                self.state = ParseState::EscIntermediate;
            }
            '7' => {
                let saved = (self.cx, self.cy, self.pen);
                if self.using_alt {
                    self.saved_alt = saved;
                } else {
                    self.saved_main = saved;
                }
                self.state = ParseState::Ground;
            }
            '8' => {
                let (x, y, pen) = if self.using_alt {
                    self.saved_alt
                } else {
                    self.saved_main
                };
                self.cx = x.min(self.cols - 1);
                self.cy = y.min(self.rows - 1);
                self.pen = pen;
                self.wrap_pending = false;
                self.state = ParseState::Ground;
            }
            'M' => {
                self.reverse_index();
                self.state = ParseState::Ground;
            }
            'D' => {
                self.line_feed();
                self.state = ParseState::Ground;
            }
            'E' => {
                self.cx = 0;
                self.line_feed();
                self.state = ParseState::Ground;
            }
            'c' => {
                let (c, r) = (self.cols, self.rows);
                *self = Self::new(c, r);
            }
            _ => {
                self.state = ParseState::Ground;
            }
        }
    }

    fn finish_osc(&mut self) {
        let raw = std::mem::take(&mut self.osc_buf);
        if let Some((code, rest)) = raw.split_once(';') {
            if code == "0" || code == "2" {
                self.title = rest.to_string();
            }
        }
        self.state = ParseState::Ground;
    }

    fn csi(&mut self, ch: char) {
        if ('\x40'..='\x7E').contains(&ch) {
            let params = std::mem::take(&mut self.csi_buf);
            self.state = ParseState::Ground;
            self.dispatch_csi(&params, ch);
        } else {
            self.csi_buf.push(ch);
        }
    }

    fn dispatch_csi(&mut self, raw: &str, final_ch: char) {
        let private = raw.starts_with('?');
        let clean = raw.trim_start_matches(['?', '>', '=']);
        let nums: Vec<usize> = if clean.is_empty() {
            Vec::new()
        } else {
            clean
                .split(';')
                .map(|s| s.parse::<usize>().unwrap_or(0))
                .collect()
        };
        let p0 = |def: usize| nums.first().copied().filter(|&v| v > 0).unwrap_or(def);
        let p1 = |def: usize| nums.get(1).copied().filter(|&v| v > 0).unwrap_or(def);

        if private {
            let set = final_ch == 'h';
            if final_ch == 'h' || final_ch == 'l' {
                for &mode in &nums {
                    match mode {
                        1 => self.app_cursor = set,
                        7 => self.auto_wrap = set,
                        25 => self.cursor_visible = set,
                        1049 | 1047 | 47 => {
                            if set {
                                self.enter_alt();
                            } else {
                                self.exit_alt();
                            }
                        }
                        2004 => self.bracketed_paste = set,
                        _ => {}
                    }
                }
            }
            return;
        }

        match final_ch {
            'A' => {
                let n = p0(1);
                self.cy = self.cy.saturating_sub(n);
                self.wrap_pending = false;
            }
            'B' => {
                let n = p0(1);
                self.cy = (self.cy + n).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'C' => {
                let n = p0(1);
                self.cx = (self.cx + n).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'D' => {
                let n = p0(1);
                self.cx = self.cx.saturating_sub(n);
                self.wrap_pending = false;
            }
            'E' => {
                let n = p0(1);
                self.cy = (self.cy + n).min(self.rows - 1);
                self.cx = 0;
                self.wrap_pending = false;
            }
            'F' => {
                let n = p0(1);
                self.cy = self.cy.saturating_sub(n);
                self.cx = 0;
                self.wrap_pending = false;
            }
            'G' | '`' => {
                self.cx = (p0(1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'd' => {
                self.cy = (p0(1) - 1).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'H' | 'f' => {
                self.cy = (p0(1) - 1).min(self.rows - 1);
                self.cx = (p1(1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'J' => {
                let mode = nums.first().copied().unwrap_or(0);
                self.erase_display(mode);
            }
            'K' => {
                let mode = nums.first().copied().unwrap_or(0);
                self.erase_line(mode);
            }
            'L' => self.insert_lines(p0(1)),
            'M' => self.delete_lines(p0(1)),
            '@' => self.insert_chars(p0(1)),
            'P' => self.delete_chars(p0(1)),
            'X' => {
                let n = p0(1);
                let cx = self.cx;
                let cy = self.cy;
                let cols = self.cols;
                let pen = self.pen;
                let row = &mut self.grid_mut()[cy];
                for x in cx..(cx + n).min(cols) {
                    row[x] = VtCell {
                        ch: ' ',
                        pen,
                        continuation: false,
                    };
                }
            }
            'S' => self.scroll_up(p0(1)),
            'T' => self.scroll_down(p0(1)),
            'r' => {
                let t = (p0(1) - 1).min(self.rows - 1);
                let b = (p1(self.rows) - 1).min(self.rows - 1);
                if b > t {
                    self.top = t;
                    self.bottom = b;
                } else {
                    self.top = 0;
                    self.bottom = self.rows - 1;
                }
                self.cx = 0;
                self.cy = self.top;
                self.wrap_pending = false;
            }
            's' => {
                let saved = (self.cx, self.cy, self.pen);
                if self.using_alt {
                    self.saved_alt = saved;
                } else {
                    self.saved_main = saved;
                }
            }
            'u' => {
                let (x, y, _) = if self.using_alt {
                    self.saved_alt
                } else {
                    self.saved_main
                };
                self.cx = x.min(self.cols - 1);
                self.cy = y.min(self.rows - 1);
                self.wrap_pending = false;
            }
            'm' => self.apply_sgr(&nums),
            _ => {}
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.wrap_pending && self.auto_wrap {
            self.wrap_pending = false;
            self.cx = 0;
            self.line_feed();
        }
        if self.cx >= self.cols {
            return;
        }
        let wide = is_wide_char(ch);
        let cx = self.cx;
        let cy = self.cy;
        let cols = self.cols;
        let pen = self.pen;
        let row = &mut self.grid_mut()[cy];
        row[cx] = VtCell {
            ch,
            pen,
            continuation: false,
        };
        if wide && cx + 1 < cols {
            row[cx + 1] = VtCell {
                ch: ' ',
                pen,
                continuation: true,
            };
            self.cx += 2;
            if self.cx >= self.cols {
                self.cx = self.cols - 1;
                self.wrap_pending = self.auto_wrap;
            }
            return;
        }
        if self.cx == self.cols - 1 {
            self.wrap_pending = self.auto_wrap;
            return;
        }
        self.cx += 1;
    }

    fn line_feed(&mut self) {
        if self.cy == self.bottom {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cy == self.top {
            self.scroll_down(1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let count = n.min(self.bottom - self.top + 1);
        let top = self.top;
        let bottom = self.bottom;
        let cols = self.cols;
        let record_history = !self.using_alt && top == 0;
        for _ in 0..count {
            let popped = self.grid_mut().remove(top);
            self.grid_mut().insert(bottom, blank_row(cols));
            if record_history {
                self.push_history(popped);
            }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let count = n.min(self.bottom - self.top + 1);
        let top = self.top;
        let bottom = self.bottom;
        let cols = self.cols;
        for _ in 0..count {
            self.grid_mut().remove(bottom);
            self.grid_mut().insert(top, blank_row(cols));
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let cols = self.cols;
        let rows = self.rows;
        let cx = self.cx;
        let cy = self.cy;
        match mode {
            0 => {
                self.erase_line(0);
                for y in (cy + 1)..rows {
                    self.grid_mut()[y] = blank_row(cols);
                }
            }
            1 => {
                for y in 0..cy {
                    self.grid_mut()[y] = blank_row(cols);
                }
                let row = &mut self.grid_mut()[cy];
                for x in 0..=cx.min(cols - 1) {
                    row[x] = VtCell::default();
                }
            }
            2 => {
                for y in 0..rows {
                    self.grid_mut()[y] = blank_row(cols);
                }
            }
            3 => {
                self.history.clear();
                for y in 0..rows {
                    self.grid_mut()[y] = blank_row(cols);
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let cols = self.cols;
        let cx = self.cx;
        let cy = self.cy;
        let row = &mut self.grid_mut()[cy];
        match mode {
            0 => {
                for x in cx..cols {
                    row[x] = VtCell::default();
                }
            }
            1 => {
                for x in 0..=cx.min(cols - 1) {
                    row[x] = VtCell::default();
                }
            }
            2 => {
                *row = blank_row(cols);
            }
            _ => {}
        }
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cy < self.top || self.cy > self.bottom {
            return;
        }
        let count = n.min(self.bottom - self.cy + 1);
        let cy = self.cy;
        let bottom = self.bottom;
        let cols = self.cols;
        for _ in 0..count {
            self.grid_mut().remove(bottom);
            self.grid_mut().insert(cy, blank_row(cols));
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cy < self.top || self.cy > self.bottom {
            return;
        }
        let count = n.min(self.bottom - self.cy + 1);
        let cy = self.cy;
        let bottom = self.bottom;
        let cols = self.cols;
        for _ in 0..count {
            self.grid_mut().remove(cy);
            self.grid_mut().insert(bottom, blank_row(cols));
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let cols = self.cols;
        let cx = self.cx;
        let cy = self.cy;
        let count = n.min(cols - cx);
        let row = &mut self.grid_mut()[cy];
        for _ in 0..count {
            row.pop();
            row.insert(cx, VtCell::default());
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let cols = self.cols;
        let cx = self.cx;
        let cy = self.cy;
        let count = n.min(cols - cx);
        let row = &mut self.grid_mut()[cy];
        for _ in 0..count {
            row.remove(cx);
            row.push(VtCell::default());
        }
    }

    fn enter_alt(&mut self) {
        if self.using_alt {
            return;
        }
        self.saved_main = (self.cx, self.cy, self.pen);
        self.using_alt = true;
        self.alt = blank_grid(self.rows, self.cols);
        self.top = 0;
        self.bottom = self.rows - 1;
        self.cx = 0;
        self.cy = 0;
        self.wrap_pending = false;
    }

    fn exit_alt(&mut self) {
        if !self.using_alt {
            return;
        }
        self.using_alt = false;
        self.alt = blank_grid(self.rows, self.cols);
        let (x, y, pen) = self.saved_main;
        self.cx = x.min(self.cols - 1);
        self.cy = y.min(self.rows - 1);
        self.pen = pen;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.wrap_pending = false;
    }

    fn apply_sgr(&mut self, nums: &[usize]) {
        if nums.is_empty() {
            self.pen = VtPen::default();
            return;
        }
        let mut i = 0;
        while i < nums.len() {
            match nums[i] {
                0 => self.pen = VtPen::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                4 => self.pen.underline = true,
                7 => self.pen.inverse = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                24 => self.pen.underline = false,
                27 => self.pen.inverse = false,
                30..=37 => self.pen.fg = Some(ansi_16(nums[i] - 30)),
                39 => self.pen.fg = None,
                40..=47 => self.pen.bg = Some(ansi_16(nums[i] - 40)),
                49 => self.pen.bg = None,
                90..=97 => self.pen.fg = Some(ansi_16(nums[i] - 90 + 8)),
                100..=107 => self.pen.bg = Some(ansi_16(nums[i] - 100 + 8)),
                38 | 48 => {
                    let is_fg = nums[i] == 38;
                    if i + 2 < nums.len() && nums[i + 1] == 5 {
                        let c = ansi_256(nums[i + 2]);
                        if is_fg {
                            self.pen.fg = Some(c);
                        } else {
                            self.pen.bg = Some(c);
                        }
                        i += 2;
                    } else if i + 4 < nums.len() && nums[i + 1] == 2 {
                        let c = Color32::from_rgb(
                            nums[i + 2].min(255) as u8,
                            nums[i + 3].min(255) as u8,
                            nums[i + 4].min(255) as u8,
                        );
                        if is_fg {
                            self.pen.fg = Some(c);
                        } else {
                            self.pen.bg = Some(c);
                        }
                        i += 4;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

fn blank_row(cols: usize) -> Vec<VtCell> {
    vec![VtCell::default(); cols]
}

fn blank_grid(rows: usize, cols: usize) -> Vec<Vec<VtCell>> {
    (0..rows).map(|_| blank_row(cols)).collect()
}

fn is_row_blank(row: &[VtCell]) -> bool {
    row.iter().all(|c| c.continuation || c.ch == ' ')
}

fn reshape_row(mut row: Vec<VtCell>, cols: usize) -> Vec<VtCell> {
    if row.len() > cols {
        row.truncate(cols);
    } else if row.len() < cols {
        row.resize(cols, VtCell::default());
    }
    row
}

fn ansi_16(idx: usize) -> Color32 {
    const TABLE: [(u8, u8, u8); 16] = [
        (0x1E, 0x24, 0x30),
        (0xF8, 0x51, 0x49),
        (0x10, 0xB9, 0x81),
        (0xD6, 0xA0, 0x34),
        (0x3B, 0x82, 0xF6),
        (0xA8, 0x55, 0xF7),
        (0x06, 0xB6, 0xD4),
        (0xD0, 0xD7, 0xDE),
        (0x63, 0x70, 0x81),
        (0xFF, 0x7B, 0x72),
        (0x34, 0xD3, 0x99),
        (0xFB, 0xBF, 0x24),
        (0x60, 0xA5, 0xFA),
        (0xC0, 0x84, 0xFC),
        (0x22, 0xD3, 0xEE),
        (0xFF, 0xFF, 0xFF),
    ];
    let (r, g, b) = TABLE[idx.min(15)];
    Color32::from_rgb(r, g, b)
}

fn ansi_256(n: usize) -> Color32 {
    if n < 16 {
        return ansi_16(n);
    }
    if n < 232 {
        let idx = n - 16;
        let r = idx / 36;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        let conv = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
        return Color32::from_rgb(conv(r), conv(g), conv(b));
    }
    let gray = (8 + (n - 232).min(23) * 10) as u8;
    Color32::from_rgb(gray, gray, gray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_feed_and_bottom_trim_resize() {
        let mut s = VtScreen::new(20, 10);
        s.feed(b"Last login\r\nroot@vps:~# ");
        assert_eq!(s.cursor(), (12, 1));
        s.resize(20, 5);
        assert_eq!(s.history().len(), 0);
        assert_eq!(s.cursor(), (12, 1));
        assert!(s.all_text().contains("root@vps:~#"));
    }
}
