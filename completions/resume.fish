# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_resume_global_optspecs
    string join \n U/up= D/down= a/agent= since= list json verbose config= confirm-always no-confirm h/help V/version
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

complete -c resume -n "__fish_resume_needs_command" -s U -l up -r
complete -c resume -n "__fish_resume_needs_command" -s D -l down -r
complete -c resume -n "__fish_resume_needs_command" -s a -l agent -r
complete -c resume -n "__fish_resume_needs_command" -l since -r
complete -c resume -n "__fish_resume_needs_command" -l config -r -F
complete -c resume -n "__fish_resume_needs_command" -l list
complete -c resume -n "__fish_resume_needs_command" -l json
complete -c resume -n "__fish_resume_needs_command" -l verbose
complete -c resume -n "__fish_resume_needs_command" -l confirm-always
complete -c resume -n "__fish_resume_needs_command" -l no-confirm
complete -c resume -n "__fish_resume_needs_command" -s h -l help -d 'Print help'
complete -c resume -n "__fish_resume_needs_command" -s V -l version -d 'Print version'
complete -c resume -n "__fish_resume_needs_command" -a "config"
complete -c resume -n "__fish_resume_needs_command" -a "completions"
complete -c resume -n "__fish_resume_needs_command" -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -s h -l help -d 'Print help'
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -f -a "example"
complete -c resume -n "__fish_resume_using_subcommand config; and not __fish_seen_subcommand_from example help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from example" -s h -l help -d 'Print help'
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "example"
complete -c resume -n "__fish_resume_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand completions" -s h -l help -d 'Print help'
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions help" -f -a "config"
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions help" -f -a "completions"
complete -c resume -n "__fish_resume_using_subcommand help; and not __fish_seen_subcommand_from config completions help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c resume -n "__fish_resume_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "example"
