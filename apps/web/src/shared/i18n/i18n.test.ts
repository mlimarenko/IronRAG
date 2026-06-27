import { afterEach, describe, expect, it } from "vitest";

import i18n from "./index";

const originalLanguage = i18n.language;

afterEach(async () => {
  await i18n.changeLanguage(originalLanguage);
});

describe("AI binding warning localization", () => {
  it("includes the missing binding count in English", async () => {
    await i18n.changeLanguage("en");

    expect(i18n.t("admin.bindingsMissing", { count: 1 })).toBe("1 binding missing");
    expect(i18n.t("admin.bindingsMissing", { count: 2 })).toBe("2 bindings missing");
  });

  it("uses Russian plural forms with the missing binding count", async () => {
    await i18n.changeLanguage("ru");

    expect(i18n.t("admin.bindingsMissing", { count: 1 })).toBe("1 привязка отсутствует");
    expect(i18n.t("admin.bindingsMissing", { count: 2 })).toBe("2 привязки отсутствуют");
    expect(i18n.t("admin.bindingsMissing", { count: 5 })).toBe("5 привязок отсутствует");
  });
});
