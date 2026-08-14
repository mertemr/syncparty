import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  type ReactNode,
} from "react";

import { dictionaries, en, type MessageKey } from "./messages";

export type { MessageKey } from "./messages";
export type { Messages } from "./messages";

/** Looks up a message, falling back to English and then to the key itself. */
export type Translate = (key: MessageKey) => string;

const TranslationContext = createContext<Translate>((key) => en[key] ?? key);

export function TranslationProvider({
  language,
  children,
}: {
  language: string;
  children: ReactNode;
}) {
  const translate = useMemo<Translate>(() => {
    // Accepts regional tags such as `tr-TR`.
    const base = language.split(/[-_]/)[0];
    const dictionary = dictionaries[base] ?? en;

    return (key) => dictionary[key] ?? en[key] ?? key;
  }, [language]);

  /**
   * Keeps `<html lang>` in step with the chosen language.
   *
   * Not cosmetic: CSS `text-transform: uppercase` is locale-sensitive, and
   * this app now sets a lot of labels in uppercase. Under `lang="en"` Turkish
   * "birlikte" uppercases to "BIRLIKTE" instead of "BİRLİKTE".
   */
  useEffect(() => {
    document.documentElement.lang = language.split(/[-_]/)[0];
  }, [language]);

  return (
    <TranslationContext.Provider value={translate}>
      {children}
    </TranslationContext.Provider>
  );
}

export function useTranslate(): Translate {
  return useContext(TranslationContext);
}
