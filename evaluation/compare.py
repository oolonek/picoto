#!/usr/bin/env python3
"""Compare paired ddMS2 feature tables by one-to-one m/z/retention-time matching.

Usage: python3 picoto/evaluation/compare.py --reference-dir msdata/processed \
    --picoto-dir /path/to/picoto-output
Requires scipy, numpy, and matplotlib. MZmine calls are a comparator, not truth.
"""

import argparse
import csv
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
from scipy.sparse import coo_matrix
from scipy.sparse.csgraph import min_weight_full_bipartite_matching


STEMS = (
    ("VGF138_A01_neg", "QC −"),
    ("VGF138_A01_pos", "QC +"),
    ("VGF140_H12_neg", "blank −"),
    ("VGF140_H12_pos", "blank +"),
    ("VGF151_E05_neg", "sample −"),
    ("VGF151_E05_pos", "sample +"),
)
RT_TOLERANCE = 15.0  # seconds; reference RT is in minutes
PPM = 10.0
ABSOLUTE_MZ = 0.003


def rows(path):
    with path.open(newline="") as file:
        yield from csv.DictReader(file, delimiter="\t" if path.suffix == ".tsv" else ",")


def read_pair(stem, reference_dir, picoto_dir):
    reference = np.array(
        [(float(row["row m/z"]), float(row["row retention time"]) * 60)
         for row in rows(reference_dir / f"{stem}_features_quant.csv")],
        dtype=float,
    ).reshape(-1, 2)
    accepted = [row for row in rows(picoto_dir / f"{stem}_features.tsv")
                if row["status"] == "accepted"]
    picoto = np.array(
        [(float(row["mz"]), float(row["rt_seconds"])) for row in accepted],
        dtype=float,
    ).reshape(-1, 2)
    quality = np.array([float(row["quality_score"]) for row in accepted])
    return reference, picoto, quality


def pair_by_coordinate(reference, picoto):
    """Minimum cost maximum-cardinality matching, with one dummy per reference."""
    nr, npic = len(reference), len(picoto)
    if not nr:
        return np.empty(0, dtype=int), np.empty(0, dtype=int)
    edge_rows = list(range(nr))
    edge_cols = [npic + i for i in range(nr)]
    # A dummy must cost more than the total cost of *all* possible real edges.
    edge_cost = [4.0 * (nr + 1)] * nr
    for i, (mz, rt) in enumerate(reference):
        tol = max(ABSOLUTE_MZ, PPM * mz * 1e-6)
        candidates = np.flatnonzero(
            (np.abs(picoto[:, 0] - mz) <= tol)
            & (np.abs(picoto[:, 1] - rt) <= RT_TOLERANCE)
        )
        for j in candidates:
            edge_rows.append(i)
            edge_cols.append(int(j))
            edge_cost.append(1.0 + abs(picoto[j, 0] - mz) / tol
                             + abs(picoto[j, 1] - rt) / RT_TOLERANCE)
    matrix = coo_matrix((edge_cost, (edge_rows, edge_cols)),
                        shape=(nr, npic + nr)).tocsr()
    refs, chosen = min_weight_full_bipartite_matching(matrix)
    real = chosen < npic
    return refs[real], chosen[real]


def make_plots(data, output_dir):
    output_dir.mkdir(parents=True, exist_ok=True)
    fig, axes = plt.subplots(3, 2, figsize=(12, 11), constrained_layout=True)
    for ax, (stem, label, reference, picoto, _quality, ref_match, pic_match) in zip(
        axes.flat, data
    ):
        leftover_ref = np.ones(len(reference), dtype=bool)
        leftover_ref[ref_match] = False
        leftover_pic = np.ones(len(picoto), dtype=bool)
        leftover_pic[pic_match] = False
        ax.scatter(picoto[leftover_pic, 1] / 60, picoto[leftover_pic, 0],
                   s=5, alpha=0.35, c="#757b87", label="picoto only", rasterized=False)
        ax.scatter(picoto[pic_match, 1] / 60, picoto[pic_match, 0],
                   s=6, alpha=0.75, c="#087f8c", label="coordinate match", rasterized=False)
        ax.scatter(reference[leftover_ref, 1] / 60, reference[leftover_ref, 0],
                   s=26, marker="x", linewidths=0.8, c="#c44e52",
                   label="MZmine only", rasterized=False)
        ax.set(title=f"{label}  ({stem})", xlabel="Retention time (min)", ylabel="m/z")
        ax.grid(alpha=0.15)
    handles, labels = axes.flat[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="upper center", ncol=3, bbox_to_anchor=(0.5, 1.025))
    fig.savefig(output_dir / "coordinate-overlap.svg", bbox_inches="tight")
    plt.close(fig)

    matched = np.concatenate([quality[pic_match] for *_, quality, _ref, pic_match in data])
    unmatched = np.concatenate([
        quality[np.setdiff1d(np.arange(len(quality)), pic_match)]
        for *_, quality, _ref, pic_match in data
    ])
    fig, ax = plt.subplots(figsize=(9, 4.5), constrained_layout=True)
    bins = np.arange(0, 22, 1)
    ax.hist([np.minimum(matched, 20.5), np.minimum(unmatched, 20.5)],
            bins=bins, weights=[
                np.ones(len(matched)) / len(matched),
                np.ones(len(unmatched)) / len(unmatched),
            ], label=["matched picoto", "picoto only"], color=["#087f8c", "#757b87"])
    ax.set_xticks([0, 5, 10, 15, 20.5], ["0", "5", "10", "15", "≥20"])
    ax.set(xlabel="picoto quality_score (heuristic, not probability)",
           ylabel="Fraction within group per score bin",
           title="Selected-feature quality; all six files")
    ax.legend()
    ax.grid(axis="y", alpha=0.2)
    fig.savefig(output_dir / "quality-distribution.svg")
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-dir", required=True, type=Path)
    parser.add_argument("--picoto-dir", required=True, type=Path)
    parser.add_argument("--output-dir", type=Path, default=Path(__file__).parent)
    args = parser.parse_args()
    data = []
    print("file,mzmine,picoto,matched,mzmine_covered,picoto_overlapping")
    totals = np.zeros(3, dtype=int)
    for stem, label in STEMS:
        reference, picoto, quality = read_pair(stem, args.reference_dir, args.picoto_dir)
        ref_match, pic_match = pair_by_coordinate(reference, picoto)
        data.append((stem, label, reference, picoto, quality, ref_match, pic_match))
        nr, npic, matched = len(reference), len(picoto), len(ref_match)
        totals += [nr, npic, matched]
        print(f"{stem},{nr},{npic},{matched},{matched / nr:.1%},{matched / npic:.1%}")
    nr, npic, matched = totals
    print(f"total,{nr},{npic},{matched},{matched / nr:.1%},{matched / npic:.1%}")
    make_plots(data, args.output_dir)


if __name__ == "__main__":
    main()
