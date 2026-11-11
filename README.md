# Exec85

A lightweight, keyboard-driven, Linux-based server OS with a 1985-style tiled interface.

> **Status:** early development. Nothing here is ready to install yet.

## What it is

Exec85 is a small operating system for home servers, built on the Linux kernel. Instead of a full desktop or a bare command line, it boots into a simple tiled interface inspired by the MS-DOS Executive from Windows 1.0: a menu bar, a drive row, and a file list you move through with the arrow keys.

The retro look is on purpose, and so is the speed. Exec85 loads only what it needs, so more of the machine is left for the things you actually run on it.

## Goals

- **Fast and light:** boot quicker and use less memory at idle than a typical Linux server install.
- **Keyboard first:** everything reachable with the arrow keys and a few shortcuts.
- **Made for small servers:** run game servers, bots and websites on old hardware, such as a laptop with its lid closed.
- **Simple setup:** an installer that asks for the basics (theme, username, optional password, device name) and a login screen on every start.

## Planned features

### System

- Installer with light and dark mode
- Login screen
- Service manager to start, stop and watch your servers
- Services keep running in the background, even with nobody logged in

### Apps

- Explorer (file manager)
- Notepad (text editor)
- Cmd (terminal)
- Paint
- System monitor
- Disk usage viewer
- Network settings
- Clock and calendar
- Calculator
- A couple of small games

## Roadmap

1. Boot a minimal Linux base in QEMU
2. Shell: menu bar, drive row and arrow-key file list
3. Installer and login screen
4. Core apps
5. Service manager
6. Test on real hardware
7. First public release

## Name

"Exec" comes from the MS-DOS Executive, and "85" from 1985, the year it was released.
