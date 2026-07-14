// Публичная точка сбора багов/фидбэка. Создайте публичный репозиторий (без кода,
// только Releases + Issues + Discussions) и укажите его здесь как "владелец/имя".
export const FEEDBACK_REPO = "slakertop1/maelstrom-releases";

/// URL для создания нового issue (баг-репорт).
export function newIssueUrl(): string {
  return `https://github.com/${FEEDBACK_REPO}/issues/new`;
}
