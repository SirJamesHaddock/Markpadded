/**
 * The languages selection translation offers. `code` is what Google's endpoint
 * and local detection use; `offline` is the code of Mozilla's model for it, or
 * null when only Google can translate it.
 */
export interface TranslateLanguage {
	code: string;
	name: string;
	/** Shown in the "from" list, where the two Portuguese targets are one language. */
	sourceName?: string;
	offline: string | null;
	/** Only a translation can be written in it; text is never detected as it. */
	targetOnly?: boolean;
}

export const TRANSLATE_LANGUAGES: readonly TranslateLanguage[] = [
	{ code: 'en', name: 'English', offline: 'en' },
	{ code: 'da', name: 'Danish', offline: 'da' },
	{ code: 'fi', name: 'Finnish', offline: 'fi' },
	{ code: 'fr', name: 'French', offline: 'fr' },
	{ code: 'de', name: 'German', offline: 'de' },
	{ code: 'it', name: 'Italian', offline: 'it' },
	{ code: 'no', name: 'Norwegian', offline: 'nb' },
	{ code: 'pl', name: 'Polish', offline: 'pl' },
	{ code: 'pt', name: 'Portuguese (Brazil)', sourceName: 'Portuguese', offline: 'pt' },
	{ code: 'pt-PT', name: 'Portuguese (Portugal)', offline: 'pt', targetOnly: true },
	{ code: 'ru', name: 'Russian', offline: 'ru' },
	{ code: 'es', name: 'Spanish', offline: 'es' },
	{ code: 'sv', name: 'Swedish', offline: 'sv' },
	{ code: 'ay', name: 'Aymara', offline: null },
	{ code: 'gn', name: 'Guaraní', offline: null },
	{ code: 'qu', name: 'Quechua', offline: null },
];

export const SOURCE_LANGUAGES = TRANSLATE_LANGUAGES.filter((language) => !language.targetOnly);

export function languageName(code: string | null | undefined, asSource = false): string {
	const language = TRANSLATE_LANGUAGES.find((l) => l.code === code);
	if (!language) return code ?? '';
	return (asSource && language.sourceName) || language.name;
}

/** The source language a translation written in `code` reads as. */
export function asSourceLanguage(code: string): string {
	return code === 'pt-PT' ? 'pt' : code;
}

/**
 * The direct model pairs Firefox's engine needs for `source` → `target`, in
 * Mozilla's codes: one pair, or two through English, since Mozilla's models
 * all translate to or from English. Null when either side has no model.
 */
export function offlinePairs(source: string, target: string): [string, string][] | null {
	const from = TRANSLATE_LANGUAGES.find((l) => l.code === source)?.offline;
	const to = TRANSLATE_LANGUAGES.find((l) => l.code === target)?.offline;
	if (!from || !to) return null;
	if (from === to) return [];
	if (from === 'en' || to === 'en') return [[from, to]];
	return [
		[from, 'en'],
		['en', to],
	];
}
