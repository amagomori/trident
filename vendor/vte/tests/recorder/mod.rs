//! Records every `Perform` call of this fork and of upstream vte 0.15.0 so the two can be
//! compared on the same input and chunking.

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Print(char),
    Execute(u8),
    Hook(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Put(u8),
    Unhook,
    Osc(Vec<Vec<u8>>, bool),
    Csi(Vec<Vec<u16>>, Vec<u8>, bool, char),
    Esc(Vec<u8>, bool, u8),
    Terminated(usize),
}

/// Logs actions; `terminated()` turns true every `stop_every` actions (0: never).
#[derive(Default)]
pub struct Recorder {
    pub actions: Vec<Action>,
    pub stop_every: usize,
    since_stop: usize,
}

impl Recorder {
    fn push(&mut self, action: Action) {
        self.actions.push(action);
        self.since_stop += 1;
    }

    fn stopped(&self) -> bool {
        self.stop_every != 0 && self.since_stop >= self.stop_every
    }
}

macro_rules! impl_perform {
    ($krate:ident) => {
        impl $krate::Perform for Recorder {
            fn print(&mut self, c: char) {
                self.push(Action::Print(c));
            }

            fn execute(&mut self, byte: u8) {
                self.push(Action::Execute(byte));
            }

            fn hook(&mut self, params: &$krate::Params, inter: &[u8], ignore: bool, c: char) {
                let params = params.iter().map(<[u16]>::to_vec).collect();
                self.push(Action::Hook(params, inter.to_vec(), ignore, c));
            }

            fn put(&mut self, byte: u8) {
                self.push(Action::Put(byte));
            }

            fn unhook(&mut self) {
                self.push(Action::Unhook);
            }

            fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
                let params = params.iter().map(|p| p.to_vec()).collect();
                self.push(Action::Osc(params, bell));
            }

            fn csi_dispatch(
                &mut self,
                params: &$krate::Params,
                inter: &[u8],
                ignore: bool,
                c: char,
            ) {
                let params = params.iter().map(<[u16]>::to_vec).collect();
                self.push(Action::Csi(params, inter.to_vec(), ignore, c));
            }

            fn esc_dispatch(&mut self, inter: &[u8], ignore: bool, byte: u8) {
                self.push(Action::Esc(inter.to_vec(), ignore, byte));
            }

            fn terminated(&self) -> bool {
                self.stopped()
            }
        }
    };
}

impl_perform!(vte);
impl_perform!(upstream);

macro_rules! run {
    ($krate:ident, $chunks:expr, $stop_every:expr) => {{
        let mut parser = $krate::Parser::new();
        let mut rec = Recorder { stop_every: $stop_every, ..Recorder::default() };
        for chunk in $chunks {
            if $stop_every == 0 {
                parser.advance(&mut rec, chunk);
                continue;
            }
            let mut rest: &[u8] = chunk;
            while !rest.is_empty() {
                let used = parser.advance_until_terminated(&mut rec, rest);
                rec.actions.push(Action::Terminated(used));
                rec.since_stop = 0;
                rest = &rest[used..];
            }
        }
        rec.actions
    }};
}

/// Splits `data` at `cuts` (sorted, deduplicated, in bounds) and asserts that the fork and
/// upstream perform the same actions, through `advance` and through `advance_until_terminated`.
pub fn assert_parity(data: &[u8], cuts: &[usize]) {
    let mut chunks = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for &cut in cuts {
        chunks.push(&data[start..cut]);
        start = cut;
    }
    chunks.push(&data[start..]);
    for stop_every in [0, 1, 3] {
        let fork = run!(vte, chunks.iter().copied(), stop_every);
        let reference = run!(upstream, chunks.iter().copied(), stop_every);
        assert_eq!(fork, reference, "input {data:?} cut at {cuts:?}, stop_every {stop_every}");
    }
}

/// Turns fuzzer bytes into an input and cut points: the first byte picks how often to cut.
pub fn split_input(raw: &[u8]) -> (&[u8], Vec<usize>) {
    let Some((&every, data)) = raw.split_first() else { return (raw, Vec::new()) };
    let every = usize::from(every % 16);
    let mut cuts = Vec::new();
    if every != 0 {
        for (i, &b) in data.iter().enumerate().skip(1) {
            if usize::from(b) % every == 0 {
                cuts.push(i);
            }
        }
    }
    (data, cuts)
}
