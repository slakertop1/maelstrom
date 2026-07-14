// Lightweight i18n. English is the source language: keys ARE the English text,
// so the code reads in English. Russian is an overlay dictionary; a missing
// Russian entry falls back to the English key. `tr()` works anywhere (incl.
// non-React code like report/chart generation); `useT()` re-renders components
// on language change.
import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { ru } from "./locale/ru";

export type Lang = "en" | "ru";

let currentLang: Lang = "en";

/// Translate an English source string to the current language.
export function tr(key: string): string {
  if (currentLang === "ru") return ru[key] ?? key;
  return key;
}

/// tr with {placeholder} substitution: tr2("{n} requests", { n: 5 }).
export function tr2(key: string, vars: Record<string, string | number>): string {
  let s = tr(key);
  for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  return s;
}

interface LangCtx {
  lang: Lang;
  setLang: (l: Lang) => void;
}

const Ctx = createContext<LangCtx>({ lang: "en", setLang: () => {} });

function readInitial(): Lang {
  try {
    const v = localStorage.getItem("lang");
    if (v === "ru" || v === "en") return v;
  } catch {
    /* ignore */
  }
  return "en";
}

export function LangProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(readInitial);
  currentLang = lang;

  const setLang = (l: Lang) => {
    currentLang = l;
    try {
      localStorage.setItem("lang", l);
    } catch {
      /* ignore */
    }
    setLangState(l);
  };

  useEffect(() => {
    currentLang = lang;
    document.documentElement.lang = lang;
  }, [lang]);

  return <Ctx.Provider value={{ lang, setLang }}>{children}</Ctx.Provider>;
}

export function useLang(): LangCtx {
  return useContext(Ctx);
}

/// Returns the translate function; subscribing to the context so the component
/// re-renders when the language changes.
export function useT(): (key: string) => string {
  useContext(Ctx);
  return tr;
}
