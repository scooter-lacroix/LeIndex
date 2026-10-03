# CoSQA retrieval test subset (vendored)

- **Source repository:** https://github.com/Jun-jie-Huang/CoCLR (official repo of the
  ACL 2021 paper "CoSQA: 20,000+ Web Queries for Code Search and Question Answering",
  Huang et al., https://aclanthology.org/2021.acl-long.442.pdf)
- **Source file:** `data/search/cosqa-retrieval-test-500.json` (500 human-annotated
  web-query/code pairs; the paper's designated retriever test split)
- **Extraction:** every 8th record of the 500-record split (63 records), retaining the
  `idx`, `doc` (query), and `code` fields verbatim. No other transformation.
- **Licensing:** the CoCLR README states "Our codes follow MIT License and our datasets
  follow Computational Use of Data Agreement (CUDA) License." The full C-UDA v1.0 text
  is vendored alongside this file as `C-UDA-1.0.md` (source:
  https://github.com/microsoft/Computational-Use-of-Data-Agreement). The data is used
  here solely for Computational Use (benchmark evaluation), per C-UDA §1.1/§2.1.
- **Upstream attribution:** CoSQA was created by Beihang University, MSRA NLC group and
  STCA NLP group; code snippets originate from the CodeSearchNet Python corpus.

This subset lets the deterministic in-tree benchmark (`src/eval/external_suite.rs`)
evaluate LeIndex retrieval against real human-annotated web queries without network
access. The full 500-record split (and the 20,604-pair corpus) can be used for
out-of-band evaluation by downloading the source repository.
