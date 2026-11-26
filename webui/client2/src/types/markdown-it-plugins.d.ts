// Type declarations for the two markdown-it plugins this page uses. Neither
// package ships its own, and `tsc` gates the build, so without these the
// imports would be untyped errors rather than plugins.
//
// markdown-it types a plugin inline — a function taking the instance and
// returning nothing — and exports no named `Plugin` alias, so the shape is
// spelled out here against the exported `MarkdownIt` class.

declare module 'markdown-it-footnote' {
  import type MarkdownIt from 'markdown-it';

  const footnote: (md: MarkdownIt) => void;
  export default footnote;
}

declare module 'markdown-it-task-lists' {
  import type MarkdownIt from 'markdown-it';

  /** The options `task-lists` reads; none are passed from this page. */
  interface TaskListsOptions {
    enabled?: boolean;
    disabled?: boolean;
    wrap?: boolean;
    label?: boolean;
  }

  const taskLists: (md: MarkdownIt, options?: TaskListsOptions) => void;
  export default taskLists;
}
