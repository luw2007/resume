# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_resume_global_optspecs
    string join \n U/up= D/down= all-worktrees a/agent= since= list json verbose config= confirm-always no-confirm man h/help V/version
end

function __fish_resume_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_resume_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_resume_using_subcommand
    set -l cmd (__fish_resume_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c resume -n "__fish_resume_needs_command" -s U -l up -d 'Include ancestor directories up to N edges, or all' -r
complete -c resume -n "__fish_resume_needs_command" -s D -l down -d 'Include descendant directories down to N edges, or all' -r
complete -c resume -n "__fish_resume_needs_command" -s a -l agent -d 'Only this agent; repeatable; replaces configured agents' -r
complete -c resume -n "__fish_resume_needs_command" -l since -d 'Only Sessions active at or after this cutoff' -r
complete -c resume -n "__fish_resume_needs_command" -l config -d 'Read this config file instead of the discovered one' -r -F
complete -c resume -n "__fish_resume_needs_command" -l all-worktrees -d 'Default Scope: include every linked Git worktree, not only the current one'
complete -c resume -n "__fish_resume_needs_command" -l list -d 'Print the plain table instead of opening the picker'
complete -c resume -n "__fish_resume_needs_command" -l json -d 'Print JSON v1 to stdout; implies --list'
complete -c resume -n "__fish_resume_needs_command" -l verbose -d 'Include redacted paths and error chains in diagnostics'
complete -c resume -n "__fish_resume_needs_command" -l confirm-always -d 'Ask for confirmation before every Resume'
complete -c resume -n "__fish_resume_needs_command" -l no-confirm -d 'Skip ordinary confirmation; risk prompts still apply'
complete -c resume -n "__fish_resume_needs_command" -l man -d 'Print the full manual page and exit'
complete -c resume -n "__fish_resume_needs_command" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c resume -n "__fish_resume_needs_command" -s V -l version -d 'Print version'
complete -c resume -n "__fish_resume_needs_command" -a "config" -d 'Inspect resume configuration'
complete -c resume -n "__fish_resume_needs_command" -a "completions" -d 'Print a shell completion script to stdout'
complete -c resume -n "__fish_resume_needs_command" -a "setup" -d 'Choose the agents Resume scans'
complete -c resume -n "__fish_resume_needs_command" -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -f -a "example" -d 'Print a commented example configuration file'
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from example" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "example" -d 'Print a commented example configuration file'
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand completions" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c resume -n "__fish_resume_using_subcommand setup" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions setup help" -f -a "config" -d 'Inspect resume configuration'
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions setup help" -f -a "completions" -d 'Print a shell completion script to stdout'
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions setup help" -f -a "setup" -d 'Choose the agents Resume scans'
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions setup help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "example" -d 'Print a commented example configuration file'
