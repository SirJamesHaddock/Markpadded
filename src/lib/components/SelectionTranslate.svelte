<script lang="ts">
	import { tick } from 'svelte';
	import { invoke } from '@tauri-apps/api/core';
	import { settings } from '../stores/settings.svelte.js';
	import {
		SOURCE_LANGUAGES,
		TRANSLATE_LANGUAGES,
		asSourceLanguage,
		languageName,
	} from '../translate/languages.js';
	import {
		detectLanguage,
		translate,
		type DetectedLanguage,
		type TranslationEngine,
	} from '../translate/translate.js';

	let {
		root,
		scroller,
		canHighlight = false,
		onhighlight,
	} = $props<{
		/** The rendered document; selections outside it are ignored. */
		root: HTMLElement | null;
		/** The preview's scrolling element, which the popup stays inside. */
		scroller: HTMLElement | null;
		canHighlight?: boolean;
		onhighlight?: () => void;
	}>();

	/** Gap between the selection and the popup, and between the popup and the pane's edge. */
	const GAP = 8;
	const EDGE = 4;
	/** Detection only needs a sample; whatlang slows down on a whole document. */
	const DETECT_SAMPLE = 1000;
	/** Pause after typing in the input before translating again. */
	const RETYPE_DELAY = 600;

	let mode = $state<'hidden' | 'toolbar' | 'window'>('hidden');
	let position = $state({ top: 0, left: 0 });
	let selectedText = $state('');
	let detected = $state<DetectedLanguage | null>(null);
	let from = $state('auto');
	let input = $state('');
	let output = $state('');
	let engine = $state<TranslationEngine | null>(null);
	let googleDetected = $state<string | null>(null);
	let error = $state('');
	let busy = $state(false);
	let popupEl = $state<HTMLElement | null>(null);
	let popupHeight = $state(0);
	let popupWidth = $state(0);

	// Read when placing the popup, never rendered.
	let range: Range | null = null;
	let detectRun = 0;
	let translateRun = 0;
	let retypeTimer: ReturnType<typeof setTimeout> | undefined;

	let detectLabel = $derived(
		from === 'auto' && googleDetected
			? `Detect (${languageName(googleDetected, true)})`
			: 'Detect language',
	);
	let engineLabel = $derived(
		busy ? 'Translating…' : engine === 'google' ? 'Google' : engine === 'offline' ? 'Offline (Firefox)' : '',
	);

	/** Moves the popup to `<body>`, out from under the panes' slide transforms. */
	function portal(node: HTMLElement) {
		document.body.appendChild(node);
		return { destroy: () => node.remove() };
	}

	/** Above the selection, or below it when there is no room above, inside the pane. */
	function place() {
		if (!range || !popupEl || mode === 'hidden') return;
		const rects = range.getClientRects();
		const first = rects.length ? rects[0] : range.getBoundingClientRect();
		const last = rects.length ? rects[rects.length - 1] : first;
		const pane = (scroller ?? document.documentElement).getBoundingClientRect();
		if (mode === 'toolbar' && (last.bottom < pane.top || first.top > pane.bottom)) {
			mode = 'hidden';
			return;
		}
		const height = popupEl.offsetHeight;
		const width = popupEl.offsetWidth;
		let top = first.top - height - GAP;
		if (top < pane.top + EDGE) top = last.bottom + GAP;
		top = Math.max(pane.top + EDGE, Math.min(top, pane.bottom - height - EDGE));
		const left = Math.max(pane.left + EDGE, Math.min(first.left, pane.right - width - EDGE));
		position = { top, left };
	}

	$effect(() => {
		void mode;
		void popupHeight;
		void popupWidth;
		place();
	});

	function close() {
		clearTimeout(retypeTimer);
		detectRun++;
		translateRun++;
		busy = false;
		mode = 'hidden';
		range = null;
	}

	function readSelection() {
		if (mode === 'window' || !root) return;
		const selection = window.getSelection();
		if (!selection || selection.isCollapsed || selection.rangeCount === 0) return;
		const current = selection.getRangeAt(0);
		if (!root.contains(current.commonAncestorContainer)) return;
		const text = selection.toString();
		if (!text.trim()) return;

		range = current.cloneRange();
		selectedText = text;
		detected = null;
		mode = 'toolbar';
		void tick().then(place);

		const run = ++detectRun;
		detectLanguage(text.trim().slice(0, DETECT_SAMPLE))
			.then((result) => {
				if (run === detectRun) detected = result;
			})
			.catch(() => {});
	}

	// Selections are read once they are finished, not while the mouse is still dragging.
	$effect(() => {
		const target = scroller;
		if (!target) return;
		const onMouseUp = (e: MouseEvent) => {
			if (e.button !== 0 || popupEl?.contains(e.target as Node)) return;
			setTimeout(readSelection, 0);
		};
		const onKeyUp = (e: KeyboardEvent) => {
			if (e.shiftKey || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a')) readSelection();
		};
		const onMove = () => place();
		document.addEventListener('mouseup', onMouseUp);
		document.addEventListener('keyup', onKeyUp);
		target.addEventListener('scroll', onMove, { passive: true });
		window.addEventListener('resize', onMove);
		return () => {
			document.removeEventListener('mouseup', onMouseUp);
			document.removeEventListener('keyup', onKeyUp);
			target.removeEventListener('scroll', onMove);
			window.removeEventListener('resize', onMove);
		};
	});

	// Another document's selection is not this one's.
	$effect(() => {
		void root;
		return () => close();
	});

	$effect(() => {
		if (mode === 'hidden') return;
		const onSelectionChange = () => {
			if (mode !== 'toolbar') return;
			if (window.getSelection()?.isCollapsed ?? true) mode = 'hidden';
		};
		const onPointerDown = (e: PointerEvent) => {
			if (!popupEl?.contains(e.target as Node)) close();
		};
		const onKeyDown = (e: KeyboardEvent) => {
			if (e.key !== 'Escape') return;
			e.stopPropagation();
			close();
		};
		document.addEventListener('selectionchange', onSelectionChange);
		document.addEventListener('pointerdown', onPointerDown, true);
		document.addEventListener('keydown', onKeyDown, true);
		return () => {
			document.removeEventListener('selectionchange', onSelectionChange);
			document.removeEventListener('pointerdown', onPointerDown, true);
			document.removeEventListener('keydown', onKeyDown, true);
		};
	});

	/** A toolbar button must not take the selection away by being pressed. */
	function keepSelection(e: MouseEvent) {
		e.preventDefault();
	}

	function copySelection() {
		void invoke('clipboard_write_text', { text: selectedText });
		mode = 'hidden';
	}

	function highlight() {
		onhighlight?.();
		mode = 'hidden';
	}

	function openTranslate() {
		input = selectedText.trim();
		from = detected?.confident ? detected.code : 'auto';
		output = '';
		error = '';
		engine = null;
		googleDetected = null;
		mode = 'window';
		void run();
	}

	async function run() {
		clearTimeout(retypeTimer);
		const text = input.trim();
		const current = ++translateRun;
		if (!text) {
			output = '';
			error = '';
			busy = false;
			return;
		}
		busy = true;
		error = '';
		try {
			const result = await translate(text, from, settings.previewTranslateTarget, detected?.code ?? null);
			if (current !== translateRun) return;
			output = result.text;
			engine = result.engine;
			googleDetected = result.detected;
		} catch (e) {
			if (current !== translateRun) return;
			error = e instanceof Error ? e.message : String(e);
			engine = null;
		} finally {
			if (current === translateRun) busy = false;
		}
	}

	function scheduleRun() {
		clearTimeout(retypeTimer);
		retypeTimer = setTimeout(run, RETYPE_DELAY);
	}

	function swap() {
		const source = from === 'auto' ? (googleDetected ?? detected?.code ?? null) : from;
		if (!source || !TRANSLATE_LANGUAGES.some((l) => l.code === source)) return;
		from = asSourceLanguage(settings.previewTranslateTarget);
		settings.previewTranslateTarget = source;
		if (output && !error) input = output;
		void run();
	}

	function copyOutput() {
		void invoke('clipboard_write_text', { text: output });
	}
</script>

{#if mode !== 'hidden'}
	<div
		use:portal
		bind:this={popupEl}
		bind:offsetHeight={popupHeight}
		bind:offsetWidth={popupWidth}
		class="selection-popup"
		class:window={mode === 'window'}
		style:top="{position.top}px"
		style:left="{position.left}px"
		role={mode === 'window' ? 'dialog' : 'toolbar'}
		aria-label={mode === 'window' ? 'Translate' : 'Selection'}>
		{#if mode === 'toolbar'}
			<button type="button" onmousedown={keepSelection} onclick={copySelection}>Copy</button>
			{#if canHighlight}
				<button type="button" onmousedown={keepSelection} onclick={highlight}>Highlight</button>
			{/if}
			<button type="button" class="translate" onmousedown={keepSelection} onclick={openTranslate}>
				Translate
				{#if detected?.confident}
					<span class="tag">{languageName(detected.code, true)}</span>
				{/if}
			</button>
		{:else}
			<div class="bar">
				<select aria-label="Translate from" bind:value={from} onchange={run}>
					<option value="auto">{detectLabel}</option>
					{#each SOURCE_LANGUAGES as language (language.code)}
						<option value={language.code}>
							{language.sourceName ?? language.name}{language.code === detected?.code ? ' · detected' : ''}
						</option>
					{/each}
				</select>
				<button type="button" class="icon" title="Swap languages" aria-label="Swap languages" onclick={swap}>⇄</button>
				<select aria-label="Translate to" bind:value={settings.previewTranslateTarget} onchange={run}>
					{#each TRANSLATE_LANGUAGES as language (language.code)}
						<option value={language.code}>{language.name}</option>
					{/each}
				</select>
				<button type="button" class="icon close" title="Close" aria-label="Close" onclick={close}>✕</button>
			</div>
			<div class="body">
				<textarea
					class="input"
					aria-label="Text to translate"
					bind:value={input}
					oninput={scheduleRun}
					rows="3"
					spellcheck="false"></textarea>
				<div class="output" class:stale={busy && output} aria-live="polite">
					{#if error}
						<span class="error">{error}</span>
					{:else if output}
						{output}
					{:else if busy}
						<span class="muted">Translating…</span>
					{/if}
				</div>
			</div>
			<div class="foot">
				<span class="muted">{engineLabel}</span>
				<button type="button" class="copy" onclick={copyOutput} disabled={!output || !!error}>Copy</button>
			</div>
		{/if}
	</div>
{/if}

<style>
	.selection-popup {
		position: fixed;
		z-index: 1000;
		display: flex;
		gap: 2px;
		padding: 4px;
		background: var(--color-canvas-default);
		color: var(--color-fg-default);
		border: 1px solid var(--color-border-default);
		border-radius: 6px;
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
		font-family: var(--win-font);
		font-size: 13px;
		line-height: 1.4;
		user-select: none;
	}

	.selection-popup.window {
		flex-direction: column;
		gap: 0;
		padding: 0;
		width: 360px;
		max-width: calc(100vw - 16px);
		user-select: text;
	}

	button {
		font: inherit;
		color: inherit;
		background: transparent;
		border: none;
		border-radius: 4px;
		padding: 5px 10px;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	button:hover:not(:disabled) {
		background: var(--color-neutral-muted);
	}

	button:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.translate {
		font-weight: 600;
	}

	.tag {
		font-size: 11px;
		font-weight: 600;
		padding: 1px 6px;
		border-radius: 10px;
		color: var(--color-accent-fg);
		background: color-mix(in srgb, var(--color-accent-fg) 14%, transparent);
	}

	.bar {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 8px 8px 10px;
		border-bottom: 1px solid var(--color-border-default);
	}

	select {
		font: inherit;
		color: inherit;
		background: var(--color-canvas-default);
		border: 1px solid var(--color-border-default);
		border-radius: 6px;
		padding: 3px 4px;
		min-width: 0;
		flex: 1;
	}

	.icon {
		padding: 4px 6px;
		color: var(--color-fg-muted);
		flex: none;
	}

	.close {
		margin-left: 2px;
	}

	.body {
		padding: 10px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.input {
		font: inherit;
		color: inherit;
		background: var(--color-canvas-subtle);
		border: 1px solid var(--color-border-default);
		border-radius: 6px;
		padding: 8px 10px;
		resize: vertical;
		min-height: 56px;
		max-height: 30vh;
	}

	.output {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		padding: 0 2px;
		min-height: 40px;
		max-height: 40vh;
		overflow-y: auto;
		user-select: text;
	}

	.output.stale {
		opacity: 0.55;
	}

	.muted {
		color: var(--color-fg-muted);
	}

	.error {
		color: var(--color-danger-fg);
	}

	.foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 6px 8px 6px 12px;
		border-top: 1px solid var(--color-border-default);
		font-size: 12px;
	}

	.copy {
		border: 1px solid var(--color-border-default);
		padding: 3px 10px;
	}
</style>
