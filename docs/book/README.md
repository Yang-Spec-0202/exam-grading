# mdBook 静态站（GitHub Pages 目标）

> 状态：M0 已可构建（mdBook 0.5.4 实测，见 `../dev/research/toolchain.md`）。

## 1. 为什么要生成 src

`docs/` 下唯一内容源是：

- `docs/README.md`（→ 书籍首页）
- `docs/user/`
- `docs/dev/`
- `docs/adr/`
- `docs/changelog.md`

mdBook 需要一个包含 `SUMMARY.md` 的 `src` 目录才能构建。为避免同一段文字维护两份，
`docs/book/src/` 由 `scripts/docs.sh` 从上述目录**生成**（生成结果入库，保证 checkout 后
直接 `mdbook build docs/book` 可构建）：

1. `docs/README.md` → `src/index.md`
2. 复制 `docs/user`、`docs/dev`、`docs/adr` 三个目录
3. `docs/changelog.md` → `src/changelog.md`
4. `SUMMARY.md`（本目录，入库）→ `src/SUMMARY.md`
5. 执行 `mdbook build docs/book`

因此：**修改文档请改 docs/user、docs/dev、docs/adr 或 docs/README.md，不要直接改 src**；
改完执行 `bash scripts/docs.sh` 重新生成，并把 `docs/book/src/` 的变更一起提交
（CI 的文档任务会校验生成结果与提交内容一致，防止书籍内容落后）。

## 2. 本地构建

新 shell 先执行 `source "$HOME/.cargo/env"`（mdBook 安装在 `~/.cargo/bin`）。

- 只需构建：`bash scripts/docs.sh`，产物在 `docs/book/book/index.html`。
- 门禁（含死链检查）：`bash scripts/docs-check.sh`。

## 3. GitHub Pages

- 构建产物目录：`docs/book/book/`（不提交）。
- 项目站点地址形如 `https://<owner>.github.io/<repo>/`，`book.toml` 的 `site-url` 必须与仓库名一致
  （当前为 `/exam-grading/`；仓库改名时同步修改）。
- 发布工作流：`.github/workflows/pages.yml`（构建 → 上传 artifact → 部署 Pages）。

## 4. 配置约束

mdBook 0.5 起**配置中的未知字段会直接报错**（见 `../dev/research/toolchain.md`）。
新增 `book.toml` 字段前先查官方文档的「General configuration」与「HTML renderer」两页，
只使用其中列出的字段；`create-missing = false` 表示 `SUMMARY.md` 列出的章节必须真实存在。
