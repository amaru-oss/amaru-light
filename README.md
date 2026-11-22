# Mini-Amaru

Runtime local e experimental em **Rust** para coordenar agentes de engenharia via CLI. SQLite + YAML + Git/GitHub CLI. O frontend React/TypeScript permanece em `web/`. Não é necessário instalar Python.

## Requisitos e instalação

- Rust e Cargo (toolchain estável com suporte à edição 2024)
- Git; GitHub CLI (`gh`) autenticado para milestones, issues e PRs
- Pelo menos uma CLI de agente instalada e autenticada
- Node.js/npm apenas para o frontend

```bash
make install
cp packaging/example.yaml projects/meu-projeto.yaml
```

O binário fica em `target/release/mini-amaru`. Use-o diretamente ou execute os atalhos `make`/`cargo run` abaixo.

## Pacote para outra máquina

```bash
make package
# Opcional: outro target, com toolchain/linker já instalados:
make package TARGET=x86_64-unknown-linux-musl
```

Gera `dist/mini-amaru-<versão>-<target>.tar.gz` e checksum SHA-256. O pacote inclui
binário release, frontend compilado, configurações YAML, projetos, skills e
instalador. Não inclui bancos, logs, credenciais, checkouts ou caches. O build
precisa de Rust/Cargo e Node.js/npm; a máquina de destino não precisa deles.
Cada pacote atende seu sistema operacional/arquitetura; cross-compilation exige
as ferramentas do target. Um pacote macOS não roda no Linux. Linux GNU também
exige glibc compatível com a máquina de build.

Valide um pacote nativo com `scripts/test-package.sh dist/<arquivo>.tar.gz`.

Copie o arquivo para a outra máquina, extraia e execute:

```bash
tar -xzf mini-amaru-<versão>-<target>.tar.gz
cd mini-amaru-<versão>-<target>
./install.sh "$HOME/bot/mini-amaru"
cd "$HOME/bot/mini-amaru"
# Configure projects/example.yaml e config/providers.yaml.
./mini-amaru doctor
./mini-amaru serve
```

Abra **http://127.0.0.1:8765**: o launcher do pacote serve a interface e a API na
mesma porta. Git, `gh` e a CLI de agente escolhida devem estar instalados e
autenticados no destino. O instalador usa um diretório gravável pelo usuário e
recusa sobrescrever instalações existentes. Instruções completas, acesso via SSH
e diagnóstico de permissões do SQLite estão em `INSTALL.md` dentro do pacote.

Edite `projects/meu-projeto.yaml`: `project.root` aponta para o checkout do projeto (relativo ao arquivo YAML ou absoluto); `project.repository` é `owner/repo`; `models.allowed` limita modelos; `execution.check_command` define a validação. Os arquivos em `config/` selecionam providers, modelos, rotas, timeouts e armazenamento. Os comandos dos providers recebem o prompt em stdin e substituem `{model}`.

Os diretórios de banco, artefatos e worktrees são criados automaticamente.
Ao iniciar um job ou loop, se `project.root` estiver ausente ou vazio e houver
`project.repository`, o runtime clona esse repositório com `gh repo clone`.
Autentique o `gh` como o usuário do serviço. Checkouts existentes com arquivos
são preservados. Sem `project.repository`, um diretório ausente é criado como
workspace vazio. `doctor` e `loop --dry-run` não clonam repositórios.

```bash
make doctor CONFIG=projects/meu-projeto.yaml
make run CONFIG=projects/meu-projeto.yaml PROMPT='Implemente a funcionalidade X com testes.'
make jobs CONFIG=projects/meu-projeto.yaml
make status CONFIG=projects/meu-projeto.yaml JOB=1
```

Equivalente via CLI:

```bash
cargo run -- -c projects/meu-projeto.yaml run -p 'Implemente a funcionalidade X com testes.'
cargo run -- -c projects/meu-projeto.yaml resume --job 1
cargo run -- --help
```

`-c/--config` pode aparecer antes ou depois do subcomando. `status` e `jobs` usam o banco da configuração selecionada.

## Dois modos de execução

**Job (`run`)**: o modelo de planejamento gera tarefas JSON. Cada tarefa roda em uma nova chamada ao provider, sequencialmente, com instruções e resumos recentes. O runtime registra tentativas, eventos, saída e relatório. Os agentes trabalham diretamente em `project.root` e recebem instruções para não fazer commit/push/merge. `resume` cria um novo job com o pedido original e instruções de continuação.

**Loop de entrega (`loop`)**: uma máquina de estados persistente observa o GitHub, escolhe trabalho elegível e coordena sua entrega:

```text
SYNC -> OBSERVE -> DECIDE -> WORK -> SYNC ...
                         |
                         PREPARE -> EXECUTE -> VERIFY -> PUBLISH -> REVIEW -> MERGE
```

O software valida as decisões do brain e usa fallback determinístico se o modelo retornar uma ação inválida ou estiver indisponível. Cada item vira um job filho em um worktree Git separado. Os agentes implementam e commitam; o runtime cria branches, executa a verificação, publica PRs, solicita review e aplica a política de merge.

O loop respeita bloqueios e bloqueios herdados do pai, sub-issues, PRs abertos e labels de portão humano (`owner-task`, `awaiting-owner-approval`, `milestone-exit`). Sub-issues usam a branch de integração do pai. PRs parciais referenciam a issue; o fechamento exige conclusão declarada e as regras de integração. Um PR do pai para a branch principal continua sendo uma decisão humana.

```bash
make loop-dry GOAL='Continue as tarefas da Milestone 5 ate concluir'
make loop GOAL='Continue as tarefas da Milestone 5 ate concluir'
cargo run -- loop -c projects/amaru.yaml --milestone 5 -p 'Continue a milestone'
cargo run -- loop -c projects/amaru.yaml --resume 12
```

`--dry-run` consulta o estado, sem agentes, worktrees, push ou PR. `Milestone N` tenta primeiro o título `M<N> ...`, antes do número interno do GitHub. Verifique a seleção no dry-run.

Políticas principais:

- `publish.mode`: `auto` ou `manual` (pausa antes da publicação).
- `merge.mode`: `manual`, `integration_only` (padrão; só integração automática) ou `always`.
- `review.enabled`, `review.max_rounds`: review independente e limite de correções.
- `loop.max_iterations`, `loop.max_tasks_per_item`, `loop.max_passes_per_item`: limites de trabalho.
- `loop.on_blocked`: `stop` ou `brain`; `loop.on_waiting`: `stop` ou `continue`.

Review e checks controlam o merge automático. A aprovação manual também exige que os checks não estejam pendentes ou falhando. O loop pode ser retomado no mesmo ID a partir de seu estado salvo. Cancelamento é cooperativo, verificado entre etapas; não interrompe imediatamente uma chamada de agente em andamento.

## API local e frontend

```bash
make serve CONFIG=projects/meu-projeto.yaml
# Ou:
cargo run -- -c projects/meu-projeto.yaml serve --port 9000 --cors-origin http://localhost:5173
```

O servidor escuta em `127.0.0.1:8765` por padrão, com um worker sequencial. As origens de browser são validadas no servidor; as origens padrão são `http://localhost:5173` e `http://127.0.0.1:5173`.

| Método | Endpoint | Uso |
| --- | --- | --- |
| GET | `/api/health` | Saúde do serviço |
| GET | `/api/jobs` | Jobs recentes |
| POST | `/api/jobs` | `{ "prompt": "...", "mode": "job" ou "loop", "milestone": "5" }` |
| GET | `/api/jobs/<id>` | Job, tarefas, itens filhos e ações disponíveis |
| GET | `/api/jobs/<id>/events?after=0` | Eventos incrementais |
| GET | `/api/jobs/<id>/log?offset=0` | Saída incremental `{text, offset, size}` |
| POST | `/api/jobs/<id>/resume` | Continua loop ou cria job de continuação |
| POST | `/api/jobs/<id>/cancel` | Solicita cancelamento do loop |
| POST | `/api/jobs/<id>/pr` | Publica PR de item pronto |
| POST | `/api/jobs/<id>/approve` | Aprova e faz merge do PR |

A criação exige `Content-Type: application/json`; o corpo tem limite de 64 KiB e o prompt de 20.000 caracteres. O frontend existente consome a mesma API:

```bash
cd web
npm install
npm run dev
```

A interface oferece dashboard, lista de execuções, formulário de objetivo, detalhes, tarefas, logs/eventos, publicação de PR, aprovação e retomada. `VITE_API_URL` substitui a URL padrão da API. Veja `web/README.md`.

Para servir o frontend compilado pelo próprio binário: `VITE_API_URL='' npm --prefix web run build`
e `cargo run -- -c projects/meu-projeto.yaml serve --web-dir web/dist`.

## Dados e configuração

- `config/runtime.yaml`: SQLite, artefatos e retries.
- `config/providers.yaml`: CLIs e endpoint Ollama (`/api/generate`).
- `config/models.yaml`: IDs lógicos de modelos.
- `config/routing.yaml`: modelo por tipo de tarefa e fallback de disponibilidade.
- `projects/*.yaml`: checkout, políticas e limites por projeto.
- `skills/`: instruções adicionadas aos prompts.
- `data/mini-amaru.db`: jobs, tarefas, tentativas, eventos, artefatos e estado do loop.
- `data/artifacts/`: planos, snapshots GitHub, relatórios e logs.

O esquema SQLite e as configurações YAML existentes são preservados. Bancos da versão anterior são migrados adicionando metadados ausentes; faça uma cópia antes da primeira execução da nova versão. Jobs que estavam RUNNING/QUEUED na inicialização do servidor viram INTERRUPTED.

## Limites operacionais

Este runtime é experimental. Worktrees separam alterações, mas não são uma sandbox. Providers e verificações rodam com as permissões do usuário e podem herdar credenciais do ambiente. Restrições nos prompts dependem do comportamento da CLI; use um ambiente isolado e permissões limitadas para conteúdo não confiável.

A API não tem autenticação; mantenha o bind local e não exponha a porta. O loop pode publicar e integrar alterações reais conforme a política. Comece com dry-run, `publish.mode: manual`, `merge.mode: manual` e limites baixos. O modo job confia no sucesso do processo do agente; um relatório não comprova correção do código.

## Desenvolvimento

```bash
make check
# cargo fmt --all -- --check
# cargo clippy --locked --all-targets -- -D warnings
# cargo test --locked
```

Os testes usam Rust, agentes shell falsos e repositórios Git temporários. Não precisam de Python, credenciais ou agentes reais. Testes de API/CLI executam o binário; os testes de entrega cobrem worktrees, publicação, review, aprovação e retomada com um GitHub CLI falso.

Estrutura Rust: `src/main.rs` (CLI), `server.rs` (HTTP), `engine.rs`/`providers.rs`/`runtime.rs` (execução), `delivery.rs` (loop e itens), `github.rs`/`gitops.rs`/`sources.rs`/`policy.rs` (integração e regras), `storage.rs`/`config.rs`/`joblog.rs` (infraestrutura).

Licença: uso experimental.
