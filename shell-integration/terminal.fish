if status is-interactive; and test (string split . -- $version)[1] -lt 4; and not set -q _terminal_integration_loaded
    set -g _terminal_integration_loaded 1

    functions -c fish_prompt _terminal_original_fish_prompt
    function _terminal_restore_status
        return $argv[1]
    end
    function fish_prompt
        set -l exit_code $status
        printf '\e]133;A\a'
        _terminal_restore_status $exit_code
        _terminal_original_fish_prompt
        printf '\e]133;B\a'
    end
    function _terminal_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end
    function _terminal_postexec --on-event fish_postexec
        set -l exit_code $status
        printf '\e]133;D;%s\a' $exit_code
    end
end
