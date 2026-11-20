This is smithay-clipboard 0.7.3 from crates.io (SHA-256
`71704c03f739f7745053bde45fa203a46c58d25bc5c4efba1d9a60e9dba81226`).
The local patch registers each live surface for file drops on the existing
clipboard seat device. A second data device can displace the first on Mutter.
The worker negotiates copy-only URI offers, reads a bounded payload through
calloop, and leaves parsing and PTY delivery to Trident. A pending read keeps a weak
reference to its original registration, never a new window at a reused address.
