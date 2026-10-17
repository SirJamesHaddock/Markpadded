/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

/*
 * Runs Firefox's offline translation engine (Bergamot) for Markpad's
 * selection translation. The model setup follows Firefox's
 * toolkit/components/translations/content/translations-engine.worker.js;
 * `bergamot-translator.js` beside this file is Firefox's emscripten glue,
 * unmodified, and the WASM and models arrive from the main thread (Rust
 * downloads and caches them).
 *
 * Messages in, each with an `id` the reply carries back:
 *   { type: 'init', wasm: ArrayBuffer }
 *   { type: 'load', key, models: [{ source, target, modelName, files: { model, lex?, vocab | srcvocab + trgvocab } }] }
 *     one model, or two when pivoting through English
 *   { type: 'translate', key, text }  → { text }
 * Replies: { id, ok: true, result } or { id, ok: false, error }.
 */

importScripts('bergamot-translator.js');

/** Alignment per file type, as Firefox's model registry requires. */
const MODEL_FILE_ALIGNMENTS = { model: 256, lex: 64, vocab: 64, srcvocab: 64, trgvocab: 64 };

/** Keeps a line's own leading and trailing whitespace around its translation. */
const WHITESPACE = /^(\s*)(.*?)(\s*)$/s;

let bergamot = null;
let service = null;
/** The loaded models and the key they were loaded for; one pair at a time. */
let loaded = { key: null, models: [] };

function initialize(wasmBinary) {
	return new Promise((resolve, reject) => {
		const module = loadBergamot({
			// Firefox's starting size: growing from small uses less than overshooting.
			INITIAL_MEMORY: 41_943_040,
			print() {},
			printErr() {},
			wasmBinary,
			onAbort() {
				reject(new Error('The offline engine failed to load.'));
			},
			onRuntimeInitialized: async () => {
				// Let `module` finish being assigned before handing it out.
				await Promise.resolve();
				resolve(module);
			},
		});
	});
}

function textConfig(entries) {
	const indent = '            ';
	return '\n' + Object.entries(entries).map(([key, value]) => `${indent}${key}: ${value}\n`).join('') + indent;
}

function buildModel({ source, target, modelName, files }) {
	const memory = {};
	for (const [fileType, buffer] of Object.entries(files)) {
		const alignment = MODEL_FILE_ALIGNMENTS[fileType];
		if (!alignment) throw new Error(`Unknown model file type: ${fileType}`);
		const aligned = new bergamot.AlignedMemory(buffer.byteLength, alignment);
		aligned.getByteArrayView().set(new Uint8Array(buffer));
		memory[fileType] = aligned;
	}

	const vocabList = new bergamot.AlignedMemoryList();
	if (memory.vocab) {
		vocabList.push_back(memory.vocab);
	} else if (memory.srcvocab && memory.trgvocab) {
		vocabList.push_back(memory.srcvocab);
		vocabList.push_back(memory.trgvocab);
	} else {
		throw new Error(`The ${source} → ${target} model has no vocabulary.`);
	}

	const config = textConfig({
		'beam-size': '1',
		normalize: '1.0',
		'word-penalty': '0',
		'max-length-break': '128',
		'mini-batch-words': '1024',
		workspace: '128',
		'max-length-factor': '2.0',
		'skip-cost': 'true',
		'cpu-threads': '0',
		quiet: 'true',
		'quiet-translation': 'true',
		'gemm-precision': modelName.endsWith('intgemm8.bin') ? 'int8shiftAll' : 'int8shiftAlphaAll',
		alignment: 'soft',
	});

	return new bergamot.TranslationModel(source, target, config, memory.model, memory.lex ?? null, vocabList, null);
}

function load(key, models) {
	if (loaded.key === key) return;
	for (const model of loaded.models) model.delete();
	// Cleared first, so a model that fails to build leaves nothing stale behind.
	loaded = { key: null, models: [] };
	loaded = { key, models: models.map(buildModel) };
}

/** Translates line by line, so paragraphs and list items keep their breaks. */
function translate(key, text) {
	if (loaded.key !== key) throw new Error('The offline model is not loaded.');
	const lines = text.split('\n').map((line) => WHITESPACE.exec(line));
	const messages = new bergamot.VectorString();
	const options = new bergamot.VectorResponseOptions();
	try {
		for (const [, , body] of lines) {
			if (!body) continue;
			messages.push_back(body);
			options.push_back({ qualityScores: false, alignment: false, html: false });
		}
		if (messages.size() === 0) return text;

		const [first, second] = loaded.models;
		const responses = second
			? service.translateViaPivoting(first, second, messages, options)
			: service.translate(first, messages, options);

		let next = 0;
		const out = lines.map(([, before, body, after]) =>
			body ? before + responses.get(next++).getTranslatedText() + after : before + after,
		);
		responses.delete();
		return out.join('\n');
	} finally {
		messages.delete();
		options.delete();
	}
}

self.onmessage = async ({ data }) => {
	const { id, type } = data;
	try {
		let result = null;
		if (type === 'init') {
			if (!bergamot) {
				bergamot = await initialize(data.wasm);
				// Caching is off in Firefox too (mozilla/firefox-translations#288).
				service = new bergamot.BlockingService({ cacheSize: 0 });
			}
		} else if (type === 'load') {
			load(data.key, data.models);
		} else if (type === 'translate') {
			result = { text: translate(data.key, data.text) };
		} else {
			throw new Error(`Unknown message: ${type}`);
		}
		self.postMessage({ id, ok: true, result });
	} catch (error) {
		self.postMessage({ id, ok: false, error: error instanceof Error ? error.message : String(error) });
	}
};
