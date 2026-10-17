/** The class `MarkdownViewer.svelte` squeezes a table by when Scroll Wide Tables is off. */
export const NEEDS_FIT_CLASS = 'needs-fit';

/**
 * Mark the tables under `root` that are too wide to fit without scrolling.
 *
 * Only those get `overflow-wrap: anywhere`. Given to every table, it also
 * re-divides the columns of one that already fits: a short column's minimum
 * width drops to about one character, and its words break (#927).
 *
 * A table is measured with its mark removed, under the default cell rule,
 * since a squeezed table always fits. The default rule wraps only between
 * words, so the table overflows its box exactly when its words cannot fit.
 * All marks come off before the first read and go back on after it: two
 * reflows, however many tables there are.
 *
 * A squeezed table is taller, and the article can grow a scrollbar that
 * narrows every other table's column. So the unmarked tables are measured
 * again with the marks in place, until none of them overflows.
 */
export function markTablesNeedingFit(root: ParentNode): void {
	let unmarked = Array.from(root.querySelectorAll<HTMLElement>('table'));
	for (const table of unmarked) table.classList.remove(NEEDS_FIT_CLASS);

	while (unmarked.length > 0) {
		const overflowing = unmarked.filter((table) => table.scrollWidth > table.clientWidth);
		if (overflowing.length === 0) return;
		for (const table of overflowing) table.classList.add(NEEDS_FIT_CLASS);
		unmarked = unmarked.filter((table) => !overflowing.includes(table));
	}
}
