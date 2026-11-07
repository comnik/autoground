#!/usr/bin/env bash
# Link skills/*/ into each detected coding agent's skills directory.
#
# Usage:
#   ./install.sh            symlink into detected agents
#   ./install.sh --all      also install for agents that are not detected
#   ./install.sh --copy     copy each skill directory instead of symlinking
#   ./install.sh --dry-run  print the actions without writing
#
# skills/*/references point at docs/brief, so all installed skills read one copy of the
# language docs. --copy snapshots those files into each skill, because the relative link
# only resolves here.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILLS_DIR="$ROOT/skills"
BRIEF_DOCS="$ROOT/docs/brief"
BRIEF_LINK="../../docs/brief"

ALL=0
COPY=0
DRY=0

usage() {
  cat <<'EOF'
Usage: ./install.sh [--all] [--copy] [--dry-run]

Link each skills/<name>/ directory that contains a SKILL.md into the skills
directory of every detected agent. Edits in this repo apply immediately.
Re-run after moving the repo. --copy writes a snapshot instead of a link.

references/ maps to docs/brief. --copy snapshots those files into each skill.
EOF
}

for arg in "$@"; do
  case "$arg" in
    --all) ALL=1 ;;
    --copy) COPY=1 ;;
    --dry-run) DRY=1 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "Unknown argument: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

have() { command -v "$1" >/dev/null 2>&1; }

# An agent counts as present when its home directory exists or its binary is on PATH.
want() { # $1 home dir, $2 binary
  [ "$ALL" = 1 ] && return 0
  [ -d "$1" ] || have "$2"
}

front_name() {
  awk '
    BEGIN { part = 0 }
    /^---[ \t]*$/ { part++; next }
    part == 1 && /^name:/ {
      sub(/^name:[ \t]*/, "")
      sub(/[ \t]*$/, "")
      gsub(/^["'\'']|["'\'']$/, "")
      print
      exit
    }
    part >= 2 { exit }
  ' "$1"
}

skills=()
if [ -d "$SKILLS_DIR" ]; then
  for dir in "$SKILLS_DIR"/*/; do
    [ -d "$dir" ] || continue
    [ -f "${dir}SKILL.md" ] || continue
    skills+=("$(basename "$dir")")
  done
fi

if [ "${#skills[@]}" -eq 0 ]; then
  echo "No skills found in $SKILLS_DIR. Each skill needs skills/<name>/SKILL.md." >&2
  exit 1
fi

for name in "${skills[@]}"; do
  declared="$(front_name "$SKILLS_DIR/$name/SKILL.md")"
  if [ -n "$declared" ] && [ "$declared" != "$name" ]; then
    echo "Warning: skills/$name/SKILL.md declares name '$declared'." >&2
  fi
done

for name in brief verify ground; do
  if [ ! -f "$SKILLS_DIR/$name/SKILL.md" ]; then
    echo "Warning: skills/$name/SKILL.md is missing." >&2
  fi
done

# True when this skill's references/ is the shared docs/brief link.
references_are_shared() { # $1 skill directory
  if [ -L "$1/references" ] && [ "$(readlink "$1/references")" = "$BRIEF_LINK" ]; then
    return 0
  fi
  return 1
}

# skills share docs/brief unless references/ is already something else.
shares_brief_docs() { # $1 skill name, $2 skill directory
  case "$1" in
    brief|verify|ground) ;;
    *)
      if references_are_shared "$2"; then
        return 0
      fi
      return 1
      ;;
  esac
  if [ ! -d "$BRIEF_DOCS" ]; then
    return 1
  fi
  if [ -e "$2/references" ] || [ -L "$2/references" ]; then
    if references_are_shared "$2"; then
      return 0
    fi
    return 1
  fi
  return 0
}

ensure_shared_brief() { # $1 skill name
  local dir="$SKILLS_DIR/$1"
  [ -f "$dir/SKILL.md" ] || return 0
  if [ ! -d "$BRIEF_DOCS" ]; then
    echo "Warning: docs/brief is missing, so skills/$1 cannot share it." >&2
    return 0
  fi
  if references_are_shared "$dir"; then
    return 0
  fi
  if [ -e "$dir/references" ] || [ -L "$dir/references" ]; then
    echo "Warning: skills/$1/references is not a link to docs/brief; left it in place." >&2
    return 0
  fi
  if [ "$DRY" = 1 ]; then
    echo "link skills/$1/references -> $BRIEF_LINK"
    return 0
  fi
  ln -s "$BRIEF_LINK" "$dir/references"
}

for name in brief verify ground; do
  ensure_shared_brief "$name"
done

place() { # $1 dest root, $2 skill name
  local src="$SKILLS_DIR/$2"
  local dest="$1/$2"
  local share=1
  if ! shares_brief_docs "$2" "$src"; then
    share=0
  fi

  if [ -L "$dest" ]; then
    :
  elif [ -e "$dest" ]; then
    if [ "$COPY" != 1 ]; then
      echo "Warning: $dest exists and is not a symlink; left it in place." >&2
      return
    fi
  fi

  if [ "$DRY" = 1 ]; then
    if [ "$COPY" = 1 ]; then
      echo "copy $src -> $dest"
      if [ "$share" = 1 ]; then
        echo "copy $BRIEF_DOCS -> $dest/references"
      fi
    else
      echo "link $dest -> $src"
    fi
    return
  fi

  mkdir -p "$1"
  if [ -L "$dest" ]; then
    rm "$dest"
  fi
  if [ "$COPY" = 1 ]; then
    # A real references/ directory would make cp keep the source symlink out.
    if [ "$share" = 1 ]; then
      rm -rf "$dest/references"
    fi
    mkdir -p "$dest"
    cp -R "$src/." "$dest/"
    if [ "$share" = 1 ]; then
      rm -rf "$dest/references"
      mkdir -p "$dest/references"
      cp -R "$BRIEF_DOCS/." "$dest/references/"
    fi
  else
    ln -s "$src" "$dest"
  fi
}

INSTALLED=()

install_into() { # $1 label, $2 skills root
  local name
  for name in "${skills[@]}"; do
    place "$2" "$name"
  done
  INSTALLED+=("$1  $2")
}

if want "$HOME/.grok" grok; then
  install_into "Grok" "$HOME/.grok/skills"
fi

if want "$HOME/.claude" claude; then
  install_into "Claude Code" "$HOME/.claude/skills"
fi

if want "$HOME/.cursor" cursor; then
  install_into "Cursor" "$HOME/.cursor/skills"
fi

if want "$HOME/.codex" codex; then
  install_into "Codex" "$HOME/.codex/skills"
fi

if want "$HOME/.hermes" hermes; then
  install_into "Hermes" "$HOME/.hermes/skills"
fi

OC_CFG="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
if want "$OC_CFG" opencode; then
  install_into "opencode" "$OC_CFG/skills"
fi

# Codex, Cursor, Gemini, Copilot, and Grok also read this vendor-neutral home.
# Grok and Claude already have their own directories, so this is only added when
# the directory is already there, or when --all is set.
if [ "$ALL" = 1 ] || [ -d "$HOME/.agents" ]; then
  install_into "Agent Skills" "$HOME/.agents/skills"
fi

if [ "${#INSTALLED[@]}" -eq 0 ]; then
  echo "No supported agents detected. Re-run with --all to install for every supported agent." >&2
  exit 1
fi

echo
printf '  %s\n' "${INSTALLED[@]}"
echo
if [ "$DRY" = 1 ]; then
  echo "Dry run. Nothing written."
elif [ "$COPY" = 1 ]; then
  echo "Copied. Re-run ./install.sh --copy after editing a skill or docs/brief."
  echo "references/ is a copy of docs/brief."
else
  echo "Linked to $SKILLS_DIR. Edits apply immediately. Re-run ./install.sh after moving this repo."
  echo "references/ symlinks to docs/brief."
fi
