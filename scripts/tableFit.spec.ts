import assert from 'node:assert/strict';

import { test } from 'vitest';

import { markTablesNeedingFit, NEEDS_FIT_CLASS } from '../src/lib/utils/tableFit.js';

/*
 * With Scroll Wide Tables off, every cell of every table used to get
 * `overflow-wrap: anywhere`. That drops a column's minimum width to about one
 * character, so in a prose table that already fit — `Status | Description` —
 * the short column was squeezed until "Completed" broke mid-word: the #927 bug
 * the base cell rule in styles.css avoids. Now only a table that would scroll
 * is squeezed.
 *
 * jsdom has no layout, so each table reports the widths a browser gives a
 * `display: block; width: max-content; max-width: 100%` table: its box is as
 * wide as its content, up to the column. Unmarked, it wraps only between words,
 * and whatever its longest words need past the column scrolls. Marked, it breaks
 * inside words and always fits — which is why it has to be measured unmarked.
 */
interface TableBox {
	id: string;
	/** Narrowest it can wrap to without breaking inside a word. */
	words: number;
	/** As wide as it is with nothing wrapped. */
	content: number;
}

interface Page {
	column: number;
	/** How much a vertical scrollbar takes off the column once some table is squeezed and taller. */
	scrollbar?: number;
}

function layOut(page: Page, boxes: TableBox[]) {
	const host = document.createElement('div');
	host.innerHTML = boxes.map(({ id }) => `<table id="${id}"><tbody><tr><td>${id}</td></tr></tbody></table>`).join('');
	const tables = Array.from(host.querySelectorAll('table'));
	const marked = (table: Element) => table.classList.contains(NEEDS_FIT_CLASS);
	const column = () => page.column - (tables.some(marked) ? (page.scrollbar ?? 0) : 0);

	tables.forEach((table, index) => {
		const { words, content } = boxes[index];
		const box = () => Math.min(content, column());
		Object.defineProperty(table, 'clientWidth', { get: box });
		Object.defineProperty(table, 'scrollWidth', {
			get: () => (marked(table) ? box() : Math.max(box(), words)),
		});
	});
	return host;
}

function markedIds(host: HTMLElement) {
	return Array.from(host.querySelectorAll(`table.${NEEDS_FIT_CLASS}`), (table) => table.id);
}

test('a table that fits is left alone, and one too wide to fit is marked', () => {
	const host = layOut({ column: 800 }, [
		{ id: 'prose', words: 260, content: 2400 },
		{ id: 'small', words: 60, content: 100 },
		{ id: 'wide', words: 1200, content: 1800 },
	]);
	markTablesNeedingFit(host);
	assert.deepEqual(markedIds(host), ['wide']);
});

test('a squeezed table that is still too wide keeps its mark', () => {
	// Squeezed, it reports that it fits. Measured that way, it would lose the
	// mark, scroll, be marked again on the next pass, and so on.
	const host = layOut({ column: 800 }, [{ id: 'wide', words: 1200, content: 1800 }]);
	markTablesNeedingFit(host);
	markTablesNeedingFit(host);
	assert.deepEqual(markedIds(host), ['wide']);
});

test('a squeezed table loses its mark once its column is wide enough', () => {
	const page = { column: 800 };
	const host = layOut(page, [{ id: 'wide', words: 1200, content: 1800 }]);
	markTablesNeedingFit(host);
	page.column = 1400;
	markTablesNeedingFit(host);
	assert.deepEqual(markedIds(host), []);
});

test('a table pushed past its column by the scrollbar a squeeze adds is marked too', () => {
	// `close` fits the 800px column with 10px to spare. Squeezing `wide` makes
	// the article tall enough to scroll, and the scrollbar takes 15px.
	const host = layOut({ column: 800, scrollbar: 15 }, [
		{ id: 'close', words: 790, content: 1500 },
		{ id: 'wide', words: 1200, content: 1800 },
	]);
	markTablesNeedingFit(host);
	assert.deepEqual(markedIds(host), ['close', 'wide']);
});

test('a document with no tables is a no-op', () => {
	const host = document.createElement('div');
	host.innerHTML = '<p>No tables here.</p>';
	markTablesNeedingFit(host);
	assert.equal(host.innerHTML, '<p>No tables here.</p>');
});
