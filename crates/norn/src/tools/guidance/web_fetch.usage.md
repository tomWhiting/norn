Every web_fetch call is processed by an extraction agent — raw page content is never returned to you. Provide `questions` as an array of strings to get specific answers from the page. If you omit questions, the extraction agent returns a general summary.

The response contains structured `answers` — an array of objects with `question` (number), `answer` (the substantive content), and `lines` (line references into the saved document). The `saved_to` field gives the path to the full markdown on disk. To read more context around an answer, open the saved file and go to the cited line numbers.

Example: to find API details from a docs page, call web_fetch with url and questions: ["What are the rate limits?", "What authentication is required?"]. The extraction agent reads the full page and returns an array of answers with line citations.

URLs must start with http:// or https://. Default timeout is 30 seconds, maximum 120. Use `detail` to control answer depth: `brief` for short answers, `normal` (default) for clear answers with context, `detailed` for comprehensive answers. Do not use for search queries — use web_search instead.
