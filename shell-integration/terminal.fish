# Fish 3 needs OSC 133/7 boundaries; fish 4 supplies native marks.
# Both versions use the prompt wrapper for on-demand editor snapshots.
if status is-interactive; and not set -q _terminal_integration_loaded
    set -g _terminal_integration_loaded 1
    set -g _terminal_native_marks 0
    if test (string split . -- $version)[1] -ge 4
        set -g _terminal_native_marks 1
    end

    function _terminal_prompt_input
        # commandline writes one final newline; preserve it and all input spaces.
        set -l cursor (commandline --cursor)
        set -l encoded (commandline --current-buffer | base64 | string join '')
        printf '\e]777;trident;prompt-input;data;%s;%s;scalar\a' $cursor "$encoded"
    end
    for mode in default insert visual replace (bind --list-modes)
        bind --mode $mode \e\[1114109u _terminal_prompt_input
    end

    function _terminal_restore_status
        return $argv[1]
    end
    function _terminal_render_prompt
        set -l exit_code $argv[1]
        if test $_terminal_native_marks = 0
            printf '\e]133;A\a'
            printf '\e]7;file://%s%s\a' $hostname (string escape --style=url -- $PWD)
        end
        _terminal_restore_status $exit_code
        _terminal_original_fish_prompt
        printf '\e]777;trident;prompt-input;ready\a'
        # Keep the adapter boundary after the entire prompt, even on native fish.
        printf '\e]133;B\a'
    end
    function _terminal_refresh_prompt --on-event fish_prompt
        set -l exit_code $status
        # Themes and ordinary function definitions can replace fish_prompt.
        set -l definition (functions fish_prompt | string collect)
        if test "$definition" != "$_terminal_wrapped_prompt"
            functions -e _terminal_original_fish_prompt
            functions -c fish_prompt _terminal_original_fish_prompt
            function fish_prompt
                _terminal_render_prompt $status
            end
            set -g _terminal_wrapped_prompt (functions fish_prompt | string collect)
        end
        return $exit_code
    end
    _terminal_refresh_prompt
    function _terminal_preexec --on-event fish_preexec
        if test $_terminal_native_marks = 0
            printf '\e]133;C\a'
        end
    end
    function _terminal_postexec --on-event fish_postexec
        set -l exit_code $status
        if test $_terminal_native_marks = 0
            printf '\e]133;D;%s\a' $exit_code
        end
    end
end
