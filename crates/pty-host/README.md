# crates/pty-host

PTY ownership and transport.

Responsibilities:

- create PTYs
- spawn child processes into PTYs
- read terminal output
- inject terminal input
- resize handling
- close/cleanup

This crate should stay focused on terminal/process mechanics, not supervisor policy.
