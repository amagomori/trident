//! XTPUSHSGR saves rendition values, never cursor positions or link-table indices.

use std::collections::VecDeque;

use vte::Params;

use crate::{Flags, Terminal, UnderlineStyle, cell::Sgr};

const DEPTH: usize = 10;

#[derive(Default)]
pub(super) struct SgrStack(VecDeque<(Selectors, Sgr)>);

struct Selectors(u32);

impl Selectors {
    const ALL: Self = Self(u32::MAX);

    fn from_params(params: &Params) -> Self {
        // vte represents a single omitted parameter as zero.
        if params.is_empty() || params.iter().eq([&[0][..]]) {
            return Self::ALL;
        }
        let mut mask = 0;
        for param in params {
            if let [n @ 1..=31] = *param {
                mask |= 1 << n;
            }
        }
        Self(mask)
    }

    fn has(&self, n: u32) -> bool {
        self.0 & (1 << n) != 0
    }

    fn underline(&self, style: Option<UnderlineStyle>) -> bool {
        match style {
            Some(UnderlineStyle::Double) => self.has(21),
            Some(_) => self.has(4),
            None => false,
        }
    }
}

impl Terminal {
    pub(super) fn push_sgr(&mut self, params: &Params) {
        let cell = &self.cursor.template;
        let saved = cell.sgr(self.underline_color(cell));
        let stack = &mut self.reports.sgr_stack.0;
        if stack.len() == DEPTH {
            stack.pop_front();
        }
        stack.push_back((Selectors::from_params(params), saved));
    }

    pub(super) fn pop_sgr(&mut self) {
        let Some((select, saved)) = self.reports.sgr_stack.0.pop_back() else {
            return;
        };
        let cell = &mut self.cursor.template;
        if select.has(30) {
            cell.fg = saved.fg;
        }
        if select.has(31) {
            cell.bg = saved.bg;
        }
        for (n, flag) in [
            (1, Flags::BOLD),
            (2, Flags::DIM),
            (3, Flags::ITALIC),
            (5, Flags::BLINK),
            (7, Flags::INVERSE),
            (8, Flags::HIDDEN),
            (9, Flags::STRIKE),
        ] {
            if select.has(n) {
                if saved.flags.contains(flag) {
                    cell.flags.insert(flag);
                } else {
                    cell.flags.remove(flag);
                }
            }
        }
        let saved_style = saved.flags.underline();
        if select.underline(saved_style) {
            cell.flags.set_underline(saved_style);
        } else if select.underline(cell.flags.underline()) {
            cell.flags.set_underline(None);
        }
        if select.0 == Selectors::ALL.0 {
            self.set_underline_color(saved.underline_color);
        }
    }
}
