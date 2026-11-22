# mini-amaru web

Frontend básico para o mini-amaru: React + TypeScript + Vite, Tailwind CSS v4, shadcn/ui, TanStack Router, Query e Table.

## Rodar

Suba a API antes (na raiz do repo):

```bash
make serve       # http://127.0.0.1:8765
```

Depois, no front:

```bash
cd web
npm install
npm run dev      # http://localhost:5173
npm run build    # gera dist/ e roda tsc -b
```

A URL da API vem de `VITE_API_URL` (padrão `http://127.0.0.1:8765`). Se a API rodar em outra porta ou origem, passe `--cors-origin` ao `serve` com a origem do front (o padrão já libera `localhost:5173`).

`make package` na raiz inclui o frontend compilado. No pacote instalado,
`./mini-amaru serve` serve a interface e a API em `http://127.0.0.1:8765`, sem Node.js.
Para o mesmo modo no checkout: `VITE_API_URL='' npm run build` neste diretório e
`cargo run -- -c projects/meu-projeto.yaml serve --web-dir web/dist` na raiz.

## Estrutura

- `src/routes/` — rotas file-based do TanStack Router: `index.tsx` (dashboard), `runs.tsx` (lista de jobs), `new.tsx` (envia prompt), `jobs.$jobId.tsx` (detalhe, tarefas, log ao vivo dos agentes e eventos). `src/routeTree.gen.ts` é gerado pelo plugin do Vite.
- `src/lib/api.ts` — cliente da API REST (`listJobs`, `getJob`, `listEvents`, `getJobLog`, `createJob`, `resumeJob`) e query options do TanStack Query. Jobs atualizam por polling; eventos são buscados com `after=<id>` e o log com `offset=<bytes>`.
- `src/components/ui/` — componentes shadcn (button, card, badge, table). Adicionar com `npm exec -- shadcn@latest add <componente>`.
- `src/lib/utils.ts` — helper `cn` do shadcn.

## Notas

- Alias `@/*` → `src/*`.
- `@tanstack/react-table` fixado em v8 (API `useReactTable`); v9 mudou a API.

## Loop de entrega na UI

`/new` tem o modo "Loop de entrega" (milestone opcional). A pagina do job de um loop lista os itens (um job por item) com
branch → base, link **View PR**, review e merge. Na pagina de um item: **Create PR**, **Approve & merge** (pede confirmacao;
e uma acao no GitHub) e a mensagem de review. No loop: **Continuar loop** (retoma o mesmo job) e **Cancelar**. As regras de
quais botoes aparecem vem do servidor (`actions` em `GET /api/jobs/<id>`).
