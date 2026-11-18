# parrot-guard

Starts a command in its own process group, and stops it when the parent process exits. On Windows it uses a job object. A `.ps1` shim that a package manager installed runs through PowerShell, since Windows cannot start the script itself.
