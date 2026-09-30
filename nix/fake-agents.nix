# Scripted stand-ins for claude / codex / kiro-cli. They discover their hook
# commands exactly the way the real CLIs do (claude: --settings file; codex:
# -c notify=[…] + ~/.codex/hooks.json; kiro: ~/.kiro/agents/*.json), then fire
# them with realistic stdin payloads, driven by lines typed into the terminal
# (so `drove send` is exercised too):
#
#   <any text>  → prompt submitted → tool start → (3s) → tool end → turn done
#   perm        → prompt → permission request; next line resolves it → turn done
#   exit        → session end, process exits
{ pkgs }:
let
  common = ''
    set -u
    SESSION="fake-$$-$RANDOM"
    log() { echo "[$(date +%T)] $*" >> /tmp/fake-agents.log; }
    # run_hook <command> <json>  — hooks are shell commands fed JSON on stdin;
    # stdout of the hook is captured (real agents inject it into context).
    run_hook() {
      [ -n "$1" ] || return 0
      out=$(printf '%s' "$2" | sh -c "$1")
      rc=$?
      log "hook rc=$rc cmd=$1 payload=$2 stdout=[$out]"
      if [ -n "$out" ]; then echo "HOOK-STDOUT-NOT-EMPTY: $out" >> /tmp/fake-agents.log; fi
    }
  '';

  claude = pkgs.writeShellScriptBin "claude" ''
    ${common}
    SETTINGS=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --settings) SETTINGS="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    log "claude start DROVE_AGENT_ID=''${DROVE_AGENT_ID:-} settings=$SETTINGS"
    hookcmd() {
      [ -n "$SETTINGS" ] || return 0
      if [ -f "$SETTINGS" ]; then src=$(cat "$SETTINGS"); else src="$SETTINGS"; fi
      printf '%s' "$src" | ${pkgs.jq}/bin/jq -r --arg e "$1" '.hooks[$e][]?.hooks[]?.command' | head -1
    }
    fire() { # fire <Event> <extra-json-object>
      payload=$(${pkgs.jq}/bin/jq -cn --arg e "$1" --arg s "$SESSION" --arg cwd "$PWD" --argjson x "$2" \
        '{hook_event_name:$e, session_id:$s, transcript_path:"/tmp/t.jsonl", cwd:$cwd} + $x')
      run_hook "$(hookcmd "$1")" "$payload"
    }
    fire SessionStart '{"source":"startup"}'
    echo "fake-claude ready ($SESSION)"
    while IFS= read -r line; do
      line=''${line%$'\r'}
      case "$line" in
        exit) fire SessionEnd '{"reason":"prompt_input_exit"}'; exit 0 ;;
        perm)
          fire UserPromptSubmit '{"prompt":"perm"}'
          fire PreToolUse '{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}'
          fire PermissionRequest '{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}'
          fire Notification '{"message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}'
          echo "Allow Bash? (type anything)"
          IFS= read -r _
          fire PostToolUse '{"tool_name":"Bash","tool_input":{},"tool_response":{}}'
          fire Stop '{"stop_hook_active":false}'
          ;;
        *)
          fire UserPromptSubmit "$(${pkgs.jq}/bin/jq -cn --arg p "$line" '{prompt:$p}')"
          fire PreToolUse '{"tool_name":"Read","tool_input":{"file_path":"README.md"}}'
          sleep 3
          fire PostToolUse '{"tool_name":"Read","tool_input":{},"tool_response":{}}'
          fire Stop '{"stop_hook_active":false}'
          echo "done: $line"
          ;;
      esac
    done
  '';

  codex = pkgs.writeShellScriptBin "codex" ''
    ${common}
    NOTIFY=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -c|--config)
          case "$2" in notify=*) NOTIFY="''${2#notify=}" ;; esac
          shift 2 ;;
        *) shift ;;
      esac
    done
    log "codex start DROVE_AGENT_ID=''${DROVE_AGENT_ID:-} notify=$NOTIFY"
    HOOKS="$HOME/.codex/hooks.json"
    hookcmd() {
      [ -f "$HOOKS" ] || return 0
      ${pkgs.jq}/bin/jq -r --arg e "$1" '(.hooks // .)[$e][]?.hooks[]?.command' "$HOOKS" | head -1
    }
    fire() {
      payload=$(${pkgs.jq}/bin/jq -cn --arg e "$1" --arg s "$SESSION" --arg cwd "$PWD" --argjson x "$2" \
        '{hook_event_name:$e, session_id:$s, turn_id:"t1", model:"fake", transcript_path:null, cwd:$cwd} + $x')
      run_hook "$(hookcmd "$1")" "$payload"
    }
    notify() {
      [ -n "$NOTIFY" ] || return 0
      mapfile -t argv < <(printf '%s' "$NOTIFY" | ${pkgs.jq}/bin/jq -r '.[]')
      msg=$(${pkgs.jq}/bin/jq -cn --arg cwd "$PWD" --arg p "$1" \
        '{type:"agent-turn-complete","thread-id":"th1","turn-id":"t1",cwd:$cwd,"input-messages":[$p],"last-assistant-message":"done"}')
      log "notify ''${argv[*]} $msg"
      "''${argv[@]}" "$msg"
    }
    fire SessionStart '{"source":"startup","permission_mode":"default"}'
    echo "fake-codex ready"
    while IFS= read -r line; do
      line=''${line%$'\r'}
      case "$line" in
        exit) fire SessionEnd '{"reason":"exit"}'; exit 0 ;;
        *)
          fire UserPromptSubmit "$(${pkgs.jq}/bin/jq -cn --arg p "$line" '{prompt:$p}')"
          fire PreToolUse '{"tool_name":"shell","tool_input":{"command":"ls"},"tool_use_id":"u1"}'
          sleep 3
          fire PostToolUse '{"tool_name":"shell","tool_input":{},"tool_response":{},"tool_use_id":"u1"}'
          fire Stop '{"stop_hook_active":false,"last_assistant_message":"done"}'
          notify "$line"
          echo "done: $line"
          ;;
      esac
    done
  '';

  kiro = pkgs.writeShellScriptBin "kiro-cli" ''
    ${common}
    AGENT=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --agent) AGENT="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    log "kiro start DROVE_AGENT_ID=''${DROVE_AGENT_ID:-} agent=$AGENT"
    files() {
      if [ -n "$AGENT" ]; then echo "$HOME/.kiro/agents/$AGENT.json"; else ls "$HOME"/.kiro/agents/*.json 2>/dev/null | head -1; fi
    }
    fire() { # camelCase events, no session id — like the real kiro-cli
      f=$(files); [ -f "$f" ] || return 0
      payload=$(${pkgs.jq}/bin/jq -cn --arg e "$1" --arg cwd "$PWD" --argjson x "$2" '{hook_event_name:$e, cwd:$cwd} + $x')
      ${pkgs.jq}/bin/jq -r --arg e "$1" '.hooks[$e][]?.command' "$f" | while IFS= read -r cmd; do
        run_hook "$cmd" "$payload"
      done
    }
    fire agentSpawn '{}'
    echo "fake-kiro ready"
    while IFS= read -r line; do
      line=''${line%$'\r'}
      case "$line" in
        exit) exit 0 ;;
        perm)
          # kiro has no permission hook: tool starts and then blocks on approval.
          fire userPromptSubmit '{"prompt":"perm"}'
          fire preToolUse '{"tool_name":"execute_bash","tool_input":{"command":"rm -rf build"}}'
          echo "Allow this action? [y/n/t]"
          IFS= read -r _
          fire postToolUse '{"tool_name":"execute_bash","tool_input":{},"tool_response":{"success":true}}'
          fire stop '{}'
          ;;
        *)
          fire userPromptSubmit "$(${pkgs.jq}/bin/jq -cn --arg p "$line" '{prompt:$p}')"
          fire preToolUse '{"tool_name":"fs_read","tool_input":{"operations":[]}}'
          sleep 2
          fire postToolUse '{"tool_name":"fs_read","tool_input":{},"tool_response":{"success":true}}'
          fire stop '{}'
          echo "done: $line"
          ;;
      esac
    done
  '';
in
{
  inherit claude codex kiro;
  all = [ claude codex kiro ];
}
