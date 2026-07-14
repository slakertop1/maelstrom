# Как раздавать Maelstrom и собирать фидбэк (код остаётся приватным)

Схема: **приватный репозиторий с кодом** (этот) + **публичный репозиторий-витрина**
(без кода — только Releases, Issues, Discussions). Сборки в публичный попадают из CI
приватного, исходники не уходят.

## Разовая настройка (~15 минут)

1. **Создать публичный репозиторий**, например `slakertop1/maelstrom-releases`
   (Public, без кода). Если имя другое — поправьте:
   - `RELEASE_REPO` в `community/release.yml`;
   - `FEEDBACK_REPO` в `src/config.ts` (куда ведёт кнопка «Сообщить об ошибке»).

2. **Наполнить публичный репозиторий** (скопировать из этой папки `community/`):
   - `README.md` → в корень (лендинг + инструкции скачивания).
   - `ISSUE_TEMPLATE/` → в `.github/ISSUE_TEMPLATE/` (шаблоны багов и идей).
   - В настройках репозитория включить **Issues** и **Discussions**.

3. **Настроить автопубликацию релизов:**
   - Скопировать `community/release.yml` → `.github/workflows/release.yml` (в приватный репо).
   - Создать fine-grained PAT с правом **Contents: Read and write** на публичный репозиторий.
   - Добавить его секретом **`RELEASE_TOKEN`** в приватный репо
     (Settings → Secrets and variables → Actions).

## Выпуск новой версии

```bash
# поднять версию в src-tauri/tauri.conf.json и src-tauri/Cargo.toml, обновить CHANGELOG.md
git tag v0.1.0
git push origin v0.1.0
```
CI соберёт Windows (MSI + портативка) и macOS (dmg) и выложит их в Releases публичного
репозитория. Пользователи скачивают оттуда.

## Где анонсировать (аудитория — разработчики / QA)

- GitHub Releases + лендинг (можно GitHub Pages из публичного репо).
- Reddit: r/devops, r/QualityAssurance, r/webdev. Hacker News (Show HN). Product Hunt — под запуск.
- Позже для удобной установки: winget (Windows), Homebrew cask (macOS).

## Доверие и продажи (на будущее)

- Подпись бинарников убирает предупреждения SmartScreen/Gatekeeper: Apple Developer ID
  (99 $/год) для macOS, code-signing сертификат для Windows. Пока бета — в README описан обход.
- Лицензия проприетарная + EULA (раз продукт будет платным).
- Для платной версии позже — лицензионные ключи/активация (отдельная задача).
