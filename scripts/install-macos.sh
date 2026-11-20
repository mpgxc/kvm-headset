#!/usr/bin/env bash
# Instala o cliente head-hub no macOS e o conecta ao hub.
#
#   scripts/install-macos.sh --hub 192.168.18.100 --token <HEADHUB_TOKEN> [--name nome-do-pc]
#
# Instala Homebrew, Rust, BlackHole (2ch e 16ch) e o `headhub`, define o BlackHole como saída e
# microfone padrão do sistema e registra o agente no login. Pode rodar de novo para atualizar.
set -euo pipefail

usage() { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
die() { printf 'erro: %s\n' "$*" >&2; exit 1; }

hub="" token="" name=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --hub) hub="${2:-}"; shift 2 ;;
    --token) token="${2:-}"; shift 2 ;;
    --name) name="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done
[[ -n "$hub" && -n "$token" ]] || usage
[[ "$(uname -s)" == Darwin ]] || die "este script é só para macOS"
cd "$(dirname "$0")/.."

step "Homebrew"
if ! command -v brew >/dev/null; then
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null || /usr/local/bin/brew shellenv)"
fi

step "Dependências (BlackHole, SwitchAudioSource, cmake); o instalador do BlackHole pede a senha"
brew install --quiet blackhole-2ch blackhole-16ch switchaudio-osx cmake

step "Rust"
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env"

step "Carregando os drivers do BlackHole"
if ! SwitchAudioSource -a -t output | grep -qx "BlackHole 2ch"; then
  sudo killall coreaudiod
  sleep 3
fi
SwitchAudioSource -a -t output | grep -qx "BlackHole 2ch" || die "BlackHole 2ch não apareceu; reinicie o Mac e rode de novo"
SwitchAudioSource -a -t input | grep -qx "BlackHole 16ch" || die "BlackHole 16ch não apareceu; reinicie o Mac e rode de novo"

step "Compilando e instalando o headhub"
cargo install --quiet --locked --path crates/client

step "Usando o headset do hub como saída e microfone padrão"
SwitchAudioSource -t output -s "BlackHole 2ch"
SwitchAudioSource -t input -s "BlackHole 16ch"

step "Configurando e iniciando o agente"
log=~/Library/Logs/headhub.log
seen="$(wc -l < "$log" 2>/dev/null || echo 0)"
setup=(headhub setup --hub "$hub" --token "$token")
[[ -n "$name" ]] && setup+=(--name "$name")
"${setup[@]}"

step "Aguardando a conexão com o hub"
name="$(sed -n 's/^name = "\(.*\)"$/\1/p' ~/.config/headhub/config.toml)"
# Up to 30 s: after a new binary, macOS blocks the first process until Local Network access is
# granted, and the agent restarts itself after 15 s. Only log lines written after setup count;
# `headhub status` would still list the previous agent for a few seconds.
for _ in $(seq 30); do
  if tail -n +"$((seen + 1))" "$log" | grep -q "headset \(neste\|em outro\) PC"; then
    headhub activate >/dev/null
    headhub status
    cat <<EOF

Pronto: '$name' está conectado e com o headset.
  - Se o macOS pedir acesso à Rede Local ou ao Microfone para o headhub, permita.
  - Trocar o headset para este Mac: headhub activate   (veja o README para criar um atalho)
  - Log do agente: ~/Library/Logs/headhub.log
  - Desfazer: headhub uninstall e escolha outra saída/entrada em Ajustes > Som.
EOF
    exit 0
  fi
  sleep 1
done
die "o agente não conectou em 30 s; veja ~/Library/Logs/headhub.log e se o hub $hub está no ar"
