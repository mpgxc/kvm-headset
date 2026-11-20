#!/usr/bin/env bash
# Builds the hub image (linux/arm64) on this machine and (re)deploys it on the server over SSH.
#
#   scripts/deploy-server.sh [usuario@host]      padrão: orangepi@192.168.18.100
#
# Na primeira execução cria server/.env com um token aleatório e instala sua chave SSH no servidor.
set -euo pipefail

SERVER="${1:-orangepi@192.168.18.100}"
REMOTE_DIR=head-hub
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }
die() { printf 'erro: %s\n' "$*" >&2; exit 1; }

command -v docker >/dev/null || die "Docker não encontrado nesta máquina (precisa para gerar a imagem)"
docker info >/dev/null 2>&1 || die "Docker não está rodando nesta máquina"

if [[ ! -f server/.env ]]; then
  step "Criando server/.env com um token novo"
  token="$(openssl rand -hex 16)"
  sed "s/^HEADHUB_TOKEN=.*/HEADHUB_TOKEN=$token/" server/.env.example > server/.env
fi

step "Acesso SSH a $SERVER"
if ! ssh -o BatchMode=yes -o ConnectTimeout=5 "$SERVER" true 2>/dev/null; then
  [[ -f ~/.ssh/id_ed25519.pub ]] || ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519
  ssh-copy-id -i ~/.ssh/id_ed25519.pub "$SERVER"
fi
ssh "$SERVER" 'docker compose version >/dev/null' || die "Docker Compose não encontrado em $SERVER"

step "Gerando a imagem headhub-hub (linux/arm64)"
docker build --platform linux/arm64 -f server/Dockerfile -t headhub-hub .

step "Enviando para $SERVER"
ssh "$SERVER" "mkdir -p ~/$REMOTE_DIR/data"
scp -q server/compose.yaml server/.env "$SERVER:$REMOTE_DIR/"
docker save headhub-hub | gzip | ssh "$SERVER" 'gunzip | docker load'

step "Iniciando o hub"
ssh "$SERVER" "cd ~/$REMOTE_DIR && docker compose up -d --force-recreate && sleep 3 && docker compose ps && docker compose logs --tail 5"

host="${SERVER#*@}"
token="$(sed -n 's/^HEADHUB_TOKEN=//p' server/.env)"
step "Pronto. Para conectar um Mac:"
echo "  scripts/install-macos.sh --hub $host --token $token"
