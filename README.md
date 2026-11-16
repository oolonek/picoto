# picoto

Picoto finds signals in a mass-spectrometry measurement and prepares them for comparison with reference collections. It does not identify chemicals or decide whether a signal came from a blank sample.

The flock turns mass-spectrometry samples and measurements into traceable evidence for possible chemical matches; it can also export a knowledge graph. Picoto prepares measured signals for comparison. Start with the [flock overview](https://github.com/oolonek/isla-gabriola-flock/blob/main/README.md) to see how the tools fit together.

The name refers to the picoto, *Carcharhinus altimus* ([Wikidata Q1118043](https://www.wikidata.org/wiki/Q1118043); [Catalogue of Life R3MC](https://www.catalogueoflife.org/data/taxon/R3MC)), the bignose shark.

[![Bignose shark swimming off Cape Lookout, North Carolina](https://commons.wikimedia.org/wiki/Special:FilePath/Bignose%20shark.jpg?width=500)](https://commons.wikimedia.org/wiki/File:Bignose_shark.jpg)

Photo: [NOAA](https://commons.wikimedia.org/wiki/File:Bignose_shark.jpg), [public domain (U.S. government work)](https://commons.wikimedia.org/wiki/Template:PD-USGov-NOAA).

## In the flock

After raw files are downloaded, Picoto prepares spectra for later comparison. Peregrino stores those results alongside sample records, and Hocofaisan searches them for **candidate hits**, not confirmed compounds. [Mato](https://github.com/oolonek/mato/blob/master/README.md) can coordinate the work.

## Start here

After the overview, follow the [processing guide](TECHNICAL.md#processing-raw-files) to see which instrument files Picoto accepts and how it produces results you can inspect. Review the results before drawing biological conclusions.

## Learn more

The [technical guide](TECHNICAL.md) covers feature selection, MS2 scan handling, output formats, terminal viewer controls, and verification limits.

Picoto is available under either the [MIT license](LICENSE-MIT) or the [Apache 2.0 license](LICENSE-APACHE).

*Naming note: For new species names, we take inspiration from [colonizer](https://crates.io/crates/colonizer)'s `inspire` mode, which suggests names from the Catalogue of Life, rather than inventing each crate name ourselves.*
