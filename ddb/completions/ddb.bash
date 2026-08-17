_ddb() {
    local current previous
    current="${COMP_WORDS[COMP_CWORD]}"
    previous="${COMP_WORDS[COMP_CWORD-1]}"
    case "${previous}" in
        --backend) COMPREPLY=( $(compgen -W "gdb lldb" -- "${current}") ); return ;;
        --api-version) COMPREPLY=( $(compgen -W "v2 v1-fallback" -- "${current}") ); return ;;
        --config|--ddb-path|--backend-log|--api-auth-token-file|--startup-report)
            COMPREPLY=( $(compgen -f -- "${current}") ); return ;;
    esac
    COMPREPLY=( $(compgen -W "serve tui launch attach connect --help --version --config --console-log" -- "${current}") )
}

_ddb_tui() {
    local current previous
    current="${COMP_WORDS[COMP_CWORD]}"
    previous="${COMP_WORDS[COMP_CWORD-1]}"
    case "${previous}" in
        --backend) COMPREPLY=( $(compgen -W "gdb lldb" -- "${current}") ); return ;;
        --api-version) COMPREPLY=( $(compgen -W "v2 v1-fallback" -- "${current}") ); return ;;
        --config|--ddb-path|--backend-log)
            COMPREPLY=( $(compgen -f -- "${current}") ); return ;;
    esac
    COMPREPLY=( $(compgen -W "launch attach connect --help --version --config --api --ddb-path --backend-log --startup-timeout" -- "${current}") )
}

complete -F _ddb ddb
complete -F _ddb_tui ddb-tui
