# Use Rust and Skim for the terminal session picker

Implement `resume` in Rust and embed Skim for streaming candidates, fuzzy filtering, selection, and session preview. This avoids rebuilding a picker directly with Ratatui and Crossterm while retaining a native single-binary CLI; agent integrations feed Skim through threads and channels, and the application does not directly depend on Ratatui, Crossterm, or Tokio. We will not fork Skim: unsupported auxiliary interactions degrade as documented rather than creating a custom TUI.
