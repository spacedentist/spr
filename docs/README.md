![spr](./spr.svg)

# Introduction

spr is a command line tool for using a stacked diff workflow with GitHub.

You do not need to have a dedicated branch in your local repository for each pull request. Instead, spr can create a pull request for an individual local commit. You can then amend and/or rebase this commit and update the pull request using spr. And finally, spr can assist you with merging the commit. It always uses "squash-merges" on GitHub, which means that the commit you worked on locally now materialises as a single commit on your centrally shared branch (traditionally called "master", or "main").

spr is heavily inspired by Phabricator, both in terms of the workflow it enables you to use, and also in terms of the terminology used ("diff", "land", etc.). If you have previously used Phabricator, you should have very little problems using spr to work with GitHub.
