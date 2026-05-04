use super::Section;

pub const SECTIONS: &[Section] = &[
    Section {
        id: "start",
        label: "Startseite",
        url: "https://taz.de",
    },
    Section {
        id: "oeko",
        label: "Oeko",
        url: "https://taz.de/Oeko/!p4610/",
    },
    Section {
        id: "oeko-oekologie",
        label: "Oeko - Oekologie",
        url: "https://taz.de/Oeko/Oekologie/!p4624/",
    },
    Section {
        id: "oeko-oekonomie",
        label: "Oeko - Oekonomie",
        url: "https://taz.de/Oeko/Oekonomie/!p4623/",
    },
    Section {
        id: "oeko-konsum",
        label: "Oeko - Konsum",
        url: "https://taz.de/Oeko/Konsum/!p4625/",
    },
    Section {
        id: "oeko-netzoekonomie",
        label: "Oeko - Netzoekonomie",
        url: "https://taz.de/Oeko/Netzoekonomie/!p4627/",
    },
    Section {
        id: "oeko-verkehr",
        label: "Oeko - Verkehr",
        url: "https://taz.de/Oeko/Verkehr/!p4628/",
    },
    Section {
        id: "oeko-arbeit",
        label: "Oeko - Arbeit",
        url: "https://taz.de/Oeko/Arbeit/!p4629/",
    },
    Section {
        id: "oeko-wissenschaft",
        label: "Oeko - Wissenschaft",
        url: "https://taz.de/Oeko/Wissenschaft/!p4636/",
    },
    Section {
        id: "politik",
        label: "Politik",
        url: "https://taz.de/Politik/!p4615/",
    },
    Section {
        id: "politik-deutschland",
        label: "Politik - Deutschland",
        url: "https://taz.de/Politik/Deutschland/!p4616/",
    },
    Section {
        id: "politik-europa",
        label: "Politik - Europa",
        url: "https://taz.de/Politik/Europa/!p4617/",
    },
    Section {
        id: "politik-amerika",
        label: "Politik - Amerika",
        url: "https://taz.de/Politik/Amerika/!p4618/",
    },
    Section {
        id: "politik-asien",
        label: "Politik - Asien",
        url: "https://taz.de/Politik/Asien/!p4619/",
    },
    Section {
        id: "politik-nahost",
        label: "Politik - Nahost",
        url: "https://taz.de/Politik/Nahost/!p4620/",
    },
    Section {
        id: "politik-afrika",
        label: "Politik - Afrika",
        url: "https://taz.de/Politik/Afrika/!p4621/",
    },
    Section {
        id: "politik-netzpolitik",
        label: "Politik - Netzpolitik",
        url: "https://taz.de/Politik/Netzpolitik/!p4622/",
    },
    Section {
        id: "gesellschaft",
        label: "Gesellschaft",
        url: "https://taz.de/Gesellschaft/!p4611/",
    },
    Section {
        id: "gesellschaft-medien",
        label: "Gesellschaft - Medien",
        url: "https://taz.de/Gesellschaft/Medien/!p4630/",
    },
    Section {
        id: "gesellschaft-alltag",
        label: "Gesellschaft - Alltag",
        url: "https://taz.de/Gesellschaft/Alltag/!p4632/",
    },
    Section {
        id: "gesellschaft-debatte",
        label: "Gesellschaft - Debatte",
        url: "https://taz.de/Gesellschaft/Debatte/!p4633/",
    },
    Section {
        id: "gesellschaft-kolumnen",
        label: "Gesellschaft - Kolumnen",
        url: "https://taz.de/Gesellschaft/Kolumnen/!p4634/",
    },
    Section {
        id: "gesellschaft-bildung",
        label: "Gesellschaft - Bildung",
        url: "https://taz.de/Gesellschaft/Bildung/!p4635/",
    },
    Section {
        id: "gesellschaft-gesundheit",
        label: "Gesellschaft - Gesundheit",
        url: "https://taz.de/Gesellschaft/Gesundheit/!p4637/",
    },
    Section {
        id: "gesellschaft-reise",
        label: "Gesellschaft - Reise",
        url: "https://taz.de/Gesellschaft/Reise/!p4638/",
    },
    Section {
        id: "gesellschaft-reportage",
        label: "Gesellschaft - Reportage und Recherche",
        url: "https://taz.de/Gesellschaft/Reportage-und-Recherche/!p5265/",
    },
    Section {
        id: "wirtschaft",
        label: "Wirtschaft",
        url: "https://taz.de/Wirtschaft/!t5008636/",
    },
    Section {
        id: "kultur",
        label: "Kultur",
        url: "https://taz.de/Kultur/!p4639/",
    },
    Section {
        id: "kultur-musik",
        label: "Kultur - Musik",
        url: "https://taz.de/Kultur/Musik/!p4640/",
    },
    Section {
        id: "kultur-film",
        label: "Kultur - Film",
        url: "https://taz.de/Kultur/Film/!p4641/",
    },
    Section {
        id: "kultur-kuenste",
        label: "Kultur - Kuenste",
        url: "https://taz.de/Kultur/Kuenste/!p4642/",
    },
    Section {
        id: "kultur-buch",
        label: "Kultur - Buch",
        url: "https://taz.de/Kultur/Buch/!p4643/",
    },
    Section {
        id: "kultur-netzkultur",
        label: "Kultur - Netzkultur",
        url: "https://taz.de/Kultur/Netzkultur/!p4631/",
    },
    Section {
        id: "wahrheit",
        label: "Wahrheit",
        url: "https://taz.de/Wahrheit/!p4644/",
    },
    Section {
        id: "sport",
        label: "Sport",
        url: "https://taz.de/Sport/!p4646/",
    },
    Section {
        id: "sport-kolumnen",
        label: "Sport - Kolumnen",
        url: "https://taz.de/Sport/Kolumnen/!p4648/",
    },
    Section {
        id: "berlin",
        label: "Berlin",
        url: "https://taz.de/Berlin/!p4649/",
    },
    Section {
        id: "nord",
        label: "Nord",
        url: "https://taz.de/Nord/!p4650/",
    },
    Section {
        id: "nord-hamburg",
        label: "Nord - Hamburg",
        url: "https://taz.de/Nord/Hamburg/!p4651/",
    },
    Section {
        id: "nord-bremen",
        label: "Nord - Bremen",
        url: "https://taz.de/Nord/Bremen/!p4652/",
    },
    Section {
        id: "nord-kultur",
        label: "Nord - Kultur",
        url: "https://taz.de/Nord/Kultur/!p4653/",
    },
    Section {
        id: "archiv",
        label: "Archiv",
        url: "https://taz.de/Archiv/!p4311/",
    },
];
