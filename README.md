# Exec85

A lightweight, keyboard-driven, Linux-based server OS with a 1985-style tiled interface.

> **Status:** early alpha. You can install Exec85 onto a disk (BIOS or UEFI), log in, and use the keyboard-driven shell with its apps (Explorer, Notepad, Terminal, Paint, System monitor, Disk usage, Network status, Clock, Calculator, games), run any program as a restart-on-crash service, install software with `apk`, use pendrives, set up a Minecraft server, and put the screen to sleep. Tested in virtual machines only; the installer erases the disk you choose, so do not use it on a real computer yet.

## What it is

Exec85 is a small, general-purpose operating system for home servers, built on the Linux kernel. Instead of a full desktop or a bare command line, it boots into a simple tiled interface inspired by the MS-DOS Executive from Windows 1.0: a menu bar, a drive row, and a file list you move through with the arrow keys.

The retro look is on purpose, and so is the speed. Exec85 loads only what it needs, so more of the machine is left for the things you actually run on it.

## Goals

- **Fast and light:** boot quicker and use less memory at idle than a typical Linux server install.
- **Keyboard first:** everything reachable with the arrow keys and a few shortcuts.
- **Run any server software:** install the programs you want and run any of them as a background service. Game servers, bots, websites, file sharing, or anything else.
- **Works on ordinary hardware:** any x86_64 PC or laptop, including old ones, such as a laptop running with its lid closed.
- **Simple setup:** an installer that asks for the basics (theme, username, optional password, device name) and a login screen on every start.

## Planned features

### System

- Installer with light and dark mode
- Login screen
- Software installation from a package manager
- Service manager to add, start, stop and watch any program as a service
- Services start at boot and keep running in the background, even with nobody logged in

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
5. Service manager and software installation
6. Test on real hardware
7. First public release

## Name

"Exec" comes from the MS-DOS Executive, and "85" from 1985, the year it was released.
