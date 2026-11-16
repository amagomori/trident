-- ../cfg/wezterm.lua plus front_end = WebGpu: WezTerm's default OpenGL front end has no pixel format on macOS VMs.
return {
  font = require('wezterm').font('JetBrains Mono'),
  font_size = 14,
  initial_cols = 100,
  initial_rows = 30,
  scrollback_lines = 10000,
  check_for_updates = false,
  enable_wayland = false,
  default_cursor_style = 'SteadyBlock',
  window_close_confirmation = 'NeverPrompt',
  window_padding = { left = 6, right = 6, top = 6, bottom = 6 },
  enable_tab_bar = false,
  front_end = 'WebGpu',
}
