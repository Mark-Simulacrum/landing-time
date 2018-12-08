set title "Pull Request merge times for rust-lang/rust"
set ylabel "Time to merge (hours)"
set xlabel "Date PR Opened"
set xdata time
set timefmt "%Y-%m-%d"
set format x "%m/%d/%y"
set datafile separator ","
set grid
# set samples 1000
set key off
datafile = 'merge_time.csv'
set yrange [0:]
plot datafile using 1:($2/60)
