import assert from 'node:assert/strict';
import { test } from 'node:test';

import { isMarkdownDocument, MARKDOWN_LINK_EXTENSIONS } from '../src/lib/utils/markdownLinks.js';

test('an untitled tab is a Markdown document', () => {
	// #965: the outline asked only about the extension, and an empty path has none.
	assert.equal(isMarkdownDocument(''), true);
});

test('a tab named with a Markdown-link extension is a Markdown document', () => {
	assert.equal(isMarkdownDocument('C:\\notes\\readme.md'), true);
	assert.equal(isMarkdownDocument('/notes/README.MD'), true);
	for (const ext of MARKDOWN_LINK_EXTENSIONS) {
		assert.equal(isMarkdownDocument(`/notes/note.${ext}`), true, `.${ext}`);
	}
});

test('a .txt tab follows the extension list rather than an answer of its own', () => {
	assert.equal(isMarkdownDocument('/notes/todo.txt'), MARKDOWN_LINK_EXTENSIONS.includes('txt'));
});

test('a tab named with any other extension, or none, is not a Markdown document', () => {
	assert.equal(isMarkdownDocument('/project/package.json'), false);
	assert.equal(isMarkdownDocument('/project/src/main.rs'), false);
	assert.equal(isMarkdownDocument('/project/Makefile'), false);
});
