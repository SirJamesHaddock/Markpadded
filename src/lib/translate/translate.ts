import { invoke } from '@tauri-apps/api/core';

import { languageName, offlinePairs } from './languages.js';

export type TranslationEngine = 'google' | 'offline';

export interface DetectedLanguage {
	code: string;
	/** Good enough to show before the user asks for a translation. */
	confident: boolean;
}

export interface TranslationResult {
	text: string;
	/** The source language Google worked from, when it was asked to detect it. */
	detected: string | null;
	engine: TranslationEngine;
}

interface GoogleTranslation {
	text: string;
	detected: string | null;
}

interface OfflineModel {
	source: string;
	target: string;
	version: string;
	files: string[];
	modelName: string;
}

export function detectLanguage(text: string): Promise<DetectedLanguage | null> {
	return invoke<DetectedLanguage | null>('detect_language', { text });
}

function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/**
 * Google first; Firefox's offline engine when Google cannot be reached or
 * answers something unusable. `from` is a language code or `'auto'`;
 * `guessedSource` is the local detection, which the offline engine needs
 * when `from` is `'auto'` because it cannot detect anything itself.
 */
export async function translate(
	text: string,
	from: string,
	to: string,
	guessedSource: string | null,
): Promise<TranslationResult> {
	let googleError: string;
	try {
		const result = await invoke<GoogleTranslation>('translate_google', { text, from, to });
		const source = from === 'auto' ? result.detected : from;
		// Ready for the next time Google is not.
		if (source) void prepareOffline(source, to).catch(() => {});
		return { text: result.text, detected: result.detected, engine: 'google' };
	} catch (error) {
		googleError = message(error);
	}

	const source = from === 'auto' ? guessedSource : from;
	if (!source) {
		throw new Error(`Google failed (${googleError}). To translate offline, pick the "from" language.`);
	}
	try {
		return { text: await translateOffline(text, source, to), detected: null, engine: 'offline' };
	} catch (error) {
		throw new Error(`Google failed (${googleError}), and so did offline translation: ${message(error)}`);
	}
}

/** Downloads, once, the models `source` → `target` needs. */
async function prepareOffline(source: string, target: string): Promise<OfflineModel[]> {
	const pairs = offlinePairs(source, target);
	if (!pairs) {
		throw new Error(`Firefox has no offline model for ${languageName(source, true)} → ${languageName(target)}`);
	}
	const models: OfflineModel[] = [];
	for (const [from, to] of pairs) {
		models.push(await invoke<OfflineModel>('offline_translation_prepare', { source: from, target: to }));
	}
	return models;
}

type WorkerReply = { id: number; ok: true; result: unknown } | { id: number; ok: false; error: string };

let worker: Worker | null = null;
let workerReady: Promise<void> | null = null;
let loadedKey: string | null = null;
let nextId = 0;
const pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();

function call<T>(message: Record<string, unknown>, transfer: Transferable[] = []): Promise<T> {
	const id = nextId++;
	return new Promise<T>((resolve, reject) => {
		pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
		worker!.postMessage({ ...message, id }, transfer);
	});
}

function resetWorker(reason: string) {
	worker?.terminate();
	worker = null;
	workerReady = null;
	loadedKey = null;
	for (const { reject } of pending.values()) reject(new Error(reason));
	pending.clear();
}

/** Starts the engine's worker and loads the WASM into it, once. */
function ensureWorker(): Promise<void> {
	if (!workerReady) {
		worker = new Worker('/translate/offline-worker.js');
		worker.onmessage = ({ data }: MessageEvent<WorkerReply>) => {
			const call = pending.get(data.id);
			if (!call) return;
			pending.delete(data.id);
			if (data.ok) call.resolve(data.result);
			else call.reject(new Error(data.error));
		};
		worker.onerror = (event) => resetWorker(event.message || 'The offline engine stopped.');
		workerReady = invoke<ArrayBuffer>('offline_translation_wasm')
			.then((wasm) => call<void>({ type: 'init', wasm }, [wasm]))
			.catch((error) => {
				resetWorker(message(error));
				throw error;
			});
	}
	return workerReady;
}

async function translateOffline(text: string, source: string, target: string): Promise<string> {
	const models = await prepareOffline(source, target);
	if (models.length === 0) return text;
	await ensureWorker();

	const key = models.map((m) => `${m.source}-${m.target}@${m.version}`).join('|');
	if (loadedKey !== key) {
		const payload = await Promise.all(
			models.map(async (m) => ({
				source: m.source,
				target: m.target,
				modelName: m.modelName,
				files: Object.fromEntries(
					await Promise.all(
						m.files.map(async (fileType) => [
							fileType,
							await invoke<ArrayBuffer>('offline_translation_file', {
								source: m.source,
								target: m.target,
								fileType,
							}),
						]),
					),
				) as Record<string, ArrayBuffer>,
			})),
		);
		await call<void>({ type: 'load', key, models: payload }, payload.flatMap((m) => Object.values(m.files)));
		loadedKey = key;
	}
	const { text: translated } = await call<{ text: string }>({ type: 'translate', key, text });
	return translated;
}
