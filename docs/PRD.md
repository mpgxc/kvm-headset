# PRD - Headset compartilhado entre 3 PCs

Nov 19, 2026 · @mpgxc

## Resumo

Um headset com um único dongle USB passa a atender três PCs (Linux, macOS e Linux). O Orange Pi, ligado o tempo todo na rede, guarda o dongle e encaminha o áudio só para o PC ativo.

Hoje o dongle funciona em um PC por vez, e a troca exige desplugar e plugar. Com esta solução, a troca passa a ser feita por atalho, sem mexer no hardware.

## Objetivos

- Trocar o headset entre os 3 PCs em até 2 segundos, por atalho.
- Latência fim a fim de até 80 ms na rede cabeada.
- Microfone e saída funcionando nos 3 PCs sem reinstalar nada a cada troca.
- Subir sozinho após reiniciar o Orange Pi.

## Fora de escopo (v1)

- Áudio de dois PCs ao mesmo tempo.
- Acesso fora da rede local.
- Driver de áudio no kernel.
- Windows.
- Mais de um headset.

## Requisitos funcionais

| ID | Requisito | Prioridade |
| --- | --- | --- |
| RF1 | Descobrir o Orange Pi na rede por mDNS, com IP fixo como alternativa | Alta |
| RF2 | Registrar cada PC cliente no hub com pareamento por token | Alta |
| RF3 | Trocar o PC ativo por atalho global, comando de terminal ou página local | Alta |
| RF4 | Enviar a saída do headset somente do PC ativo | Alta |
| RF5 | Enviar o microfone do headset a todos os clientes | Alta |
| RF6 | Criar microfone e saída virtuais em cada cliente | Alta |
| RF7 | Reconectar sozinho após queda de rede ou reinício do hub | Alta |
| RF8 | Mostrar qual PC está ativo e as métricas de latência e perda | Média |
| RF9 | Iniciar automaticamente no boot (systemd e launchd) | Média |

## Requisitos não funcionais

| ID | Requisito | Meta |
| --- | --- | --- |
| RNF1 | Latência fim a fim na rede cabeada | Até 80 ms |
| RNF2 | Codec de áudio | Opus, 48 kHz |
| RNF3 | Perda de pacotes tolerada sem falha audível | Até 1% |
| RNF4 | Troca de PC, do comando até o áudio no novo PC | Até 2 s |
| RNF5 | Criptografia do tráfego | DTLS-SRTP (WebRTC) |
| RNF6 | Uso de CPU no Orange Pi com um stream ativo | Até 25% de um núcleo (a validar) |
| RNF7 | Serviço do hub reinicia sozinho em caso de falha | systemd com Restart=always |

## Arquitetura

O Orange Pi substitui o PC hub do diagrama anterior: fica ligado e não depende de um dos PCs.

| Componente | Onde roda | Responsabilidade |
| --- | --- | --- |
| Agente de áudio do hub | Orange Pi | Captura o mic do headset, toca na saída do headset e encaminha o áudio do PC ativo |
| Sinalização | Orange Pi | WebSocket para registro de clientes, troca do PC ativo e heartbeat |
| Conexão WebRTC por cliente | Orange Pi e cada PC | Transporta o áudio em Opus, com DTLS-SRTP |
| Agente cliente | Cada PC | Lê a saída do cabo virtual, envia ao hub e alimenta o microfone virtual |
| Dispositivos virtuais | Cada PC | Microfone e saída virtuais que os apps usam |
| Descoberta | Orange Pi e PCs | mDNS (avahi), com IP fixo de reserva |

## Stack e decisões em aberto

| Decisão | Opções | Situação |
| --- | --- | --- |
| Linguagem do agente | Rust (cpal, webrtc-rs) ou TypeScript (werift, com addon nativo para áudio) | Aberta; decidir no ADR-001 |
| Biblioteca WebRTC em Node | werift ou node-datachannel; wrtc está defasado | Aberta, se a escolha for TypeScript |
| Cabo virtual no Linux | PipeWire com null sink e module-loopback | Proposta |
| Cabo virtual no macOS | BlackHole, em duas instâncias | Proposta |
| Sinalização | WebSocket | Proposta |
| Execução como serviço | systemd no Orange Pi e nos PCs Linux; launchd no macOS | Proposta |

## Riscos e mitigações

| Risco | Impacto | Mitigação |
| --- | --- | --- |
| Dongle sem suporte a USB Audio Class no Linux ARM | Alto: sem headset, o projeto para | Validar na fase 0 com lsusb, aplay e arecord |
| Latência acima de 80 ms no Orange Pi | Médio | Medir na fase 1, usar Ethernet e ajustar o frame do Opus |
| Drift de clock entre o dongle e os PCs | Médio: estalos e buffer crescendo | Buffer adaptativo e reamostragem |
| Permissões de microfone no macOS | Médio | Documentar o passo de permissão e testar a instalação |
| Estalo na troca de PC | Baixo | Fade curto de 20 a 50 ms na troca |
| CPU ou RAM insuficientes no Orange Pi | Médio | Medir o RNF6 cedo e trocar a linguagem do agente se preciso |

## Fases de entrega

1. **Fase 0, validar o hardware:** confirmar que o dongle aparece no Orange Pi (lsusb, aplay -l, arecord -l) e grava e toca sem erro. É o go/no-go do projeto.
2. **Fase 1, prova de conceito:** Orange Pi e um PC Linux trocam áudio por WebRTC, sem cabo virtual. Medir latência.
3. **Fase 2, cabos virtuais:** Linux e macOS com microfone e saída virtuais.
4. **Fase 3, vários clientes:** troca do PC ativo por atalho.
5. **Fase 4, operação:** descoberta mDNS, reconexão, início automático e métricas.
6. **Fase 5, endurecimento:** pareamento seguro, logs e instalador.

## Métricas de sucesso

- A troca funciona nos 3 PCs em 20 de 20 tentativas seguidas.
- Latência medida abaixo de 80 ms em 95% das amostras na rede cabeada.
- Nenhuma intervenção manual em 7 dias de uso.
- Reconexão automática em até 10 s após o reinício do Orange Pi.

## Perguntas abertas

Respostas que mudam o escopo, a serem definidas antes da fase 1.

- [ ] Qual modelo do Orange Pi, e ele tem porta Ethernet?
- [ ] Qual modelo do headset e do dongle (VID:PID do lsusb)?
- [ ] Os PCs estão ligados por cabo ou por Wi-Fi?
- [ ] Linguagem do agente: Rust ou TypeScript?
- [ ] Como trocar o PC ativo: atalho, bandeja ou celular?
- [ ] Quando o PC inativo toca áudio, fica mudo ou usa uma saída local?
