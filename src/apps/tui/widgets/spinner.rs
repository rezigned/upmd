/// Animated progress indicator driven by a frame tick counter.
///
/// Defaults to braille spinner. Can be configured for a simple dot spinner.
///
/// The caller must advance the tick counter via [`tick()`](Spinner::tick)
/// on every UI frame (driven by `Msg::Tick` at ~100ms intervals).
#[derive(Debug, Clone)]
pub struct Spinner {
    tick_count: u64,
    chars: &'static [char],
    step_ticks: u64,
}

impl Spinner {
    /// Creates a spinner with the given character set and tick interval.
    ///
    /// `chars` is the cycle of characters to display.
    /// `step_ticks` is the number of [`tick()`](Spinner::tick) calls between each
    ///   character advance.
    pub fn new(chars: &'static [char], step_ticks: u64) -> Self {
        Self {
            tick_count: 0,
            chars,
            step_ticks,
        }
    }

    /// Braille spinner (8-frame cycle, advances every tick).
    pub fn braille() -> Self {
        Self::new(&['⠲', '⠰', '⠴', '⠤', '⠦', '⠆', '⠖', '⠒'], 1)
    }

    /// Simple dot spinner (2-frame cycle, advances every 5 ticks = 500ms).
    pub fn dot() -> Self {
        Self::new(&['•', ' '], 5)
    }

    /// Advances the tick counter by one.
    pub fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);
    }

    /// Returns the current character based on the tick count.
    pub fn render(&self) -> char {
        let idx = (self.tick_count / self.step_ticks) as usize % self.chars.len();
        self.chars[idx]
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::braille()
    }
}
