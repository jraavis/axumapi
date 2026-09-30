# axumapi documentation site

Astro Starlight sources for https://jraavis.github.io/axumapi/.

```bash
cd website
bun install
bun run dev
```

| Command | Action |
|---|---|
| `bun run dev` | Dev server at http://localhost:4321/axumapi/ |
| `bun run build` | Production build into `dist/` |
| `bun run preview` | Preview the production build |

GitHub Actions (`.github/workflows/pages.yml`) builds this site and copies
`cargo doc --workspace --no-deps --all-features` to `dist/api/`.

Content lives in `src/content/docs/`. The sidebar is `astro.config.mjs`.
Theme tokens are `src/styles/theme.css`.
