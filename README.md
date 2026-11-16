# picoto

Picoto finds signals in a mass-spectrometry measurement and prepares them for comparison with reference collections. It does not identify chemicals or decide whether a signal came from a blank sample.

The name refers to the picoto, *Carcharhinus altimus* ([Wikidata Q1118043](https://www.wikidata.org/wiki/Q1118043); [Catalogue of Life R3MC](https://www.catalogueoflife.org/data/taxon/R3MC)), the bignose shark.

[![Bignose shark swimming off Cape Lookout, North Carolina](https://commons.wikimedia.org/wiki/Special:FilePath/Bignose%20shark.jpg?width=500)](https://commons.wikimedia.org/wiki/File:Bignose_shark.jpg)

Photo: [NOAA](https://commons.wikimedia.org/wiki/File:Bignose_shark.jpg), [public domain (U.S. government work)](https://commons.wikimedia.org/wiki/Template:PD-USGov-NOAA).

## In the flock

After raw files are downloaded, Picoto finds features and prepares spectra for later comparison. Peregrino stores those results alongside sample records, and Hocofaisan searches them for **candidate hits**, not confirmed compounds. [Mato](../mato/README.md) can coordinate the work.

## Start here

Follow the [processing guide](TECHNICAL.md#processing-raw-files) to see which instrument files Picoto accepts and how it produces results you can inspect. Review the results before drawing biological conclusions.

## Learn more

The [technical guide](TECHNICAL.md) covers feature selection, MS2 scan handling, output formats, terminal viewer controls, and verification limits.
