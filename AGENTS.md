Only add comments to code where it adds context that is hard to infer. Motivation and
historic context is reserved for commit messages.

Only add unit tests when it helps scope out and pin down an interface. Don't add unit tests
just for the sake of full coverage. Never add tests that make assertions about historic
states of the codebase, e.g. by checking the absence of a method or property.

I want to write the README and other user-facing communication in the CLI myself. Please
don't modify writing that a user will see without checking with me.